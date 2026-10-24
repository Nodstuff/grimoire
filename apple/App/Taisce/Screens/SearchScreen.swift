import SwiftUI
import TaisceKit

/// Search + Ask: docs (server search, the cached FTS index offline),
/// books (the server's shelf search), and Ask (an answer doc written from
/// your docs, with the docs it rests on). The field is the system's: at
/// the bottom of the Search tab on iPhone, in the toolbar on iPad and Mac.
struct SearchScreen: View {
    @Environment(AppModel.self) private var model
    @Environment(Router.self) private var router
    @State private var state: SearchState?
    @State private var tag: String?
    /// workspaces: false = this workspace only
    @State private var everywhere = false
    @State private var scope: SearchScope = .docs
    @State private var books: SearchBooks = .idle
    @State private var ask: AskState = .idle
    @FocusState private var focused: Bool
    /// kept for the split view's call site; the field is always the system's now
    var showsField = true

    var body: some View {
        @Bindable var router = router
        SearchContent(
            query: router.searchQuery,
            scope: $scope,
            state: state,
            tag: $tag,
            everywhere: model.hasWorkspaces ? $everywhere : nil,
            books: books,
            ask: ask,
            onOpen: { router.open(.doc($0)) },
            onOpenBook: { router.open(.book($0)) },
            onAsk: runAsk
        )
        .searchable(text: $router.searchQuery, placement: .automatic, prompt: "Search docs, books, or ask")
        .searchFocused($focused)
        .onSubmit(of: .search) { if scope == .ask { runAsk() } }
        .onChange(of: router.searchFocusRequest, initial: true) { _, n in if n > 0 { focused = true } }
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
        .task(id: BooksKey(query: router.searchQuery, scope: scope)) {
            guard scope == .books else { return }
            try? await Task.sleep(for: .milliseconds(250))
            guard !Task.isCancelled else { return }
            books = await SearchBooks.load(router.searchQuery, api: model.api)
        }
        // a new question forgets the old answer
        .onChange(of: router.searchQuery) { _, q in
            if case .answer(let a) = ask, a.question != q { ask = .idle }
            if case .nothing(let old) = ask, old != q { ask = .idle }
        }
    }

    private func runAsk() {
        let q = router.searchQuery.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !q.isEmpty, let api = model.api else { return }
        scope = .ask
        ask = .asking(q)
        Task { @MainActor in
            ask = await AskState.run(q, api: api)
            // the synthesis lands a few seconds after the doc: follow it
            var tries = 0
            while case .answer(let a) = ask, a.pending, tries < 30, router.searchQuery.trimmingCharacters(in: .whitespacesAndNewlines) == q {
                try? await Task.sleep(for: .seconds(3))
                tries += 1
                if let next = await AskState.reload(a, api: api) { ask = .answer(next) }
            }
        }
    }
}

private struct SearchKey: Hashable {
    var query: String
    var everywhere: Bool
    var workspace: WorkspaceScope?
}

private struct BooksKey: Hashable {
    var query: String
    var scope: SearchScope
}

enum SearchScope: String, CaseIterable, Hashable {
    case docs, books, ask

    var title: String {
        switch self {
        case .docs: "Docs"
        case .books: "Books"
        case .ask: "Ask"
        }
    }
}

/// Books matching the query (`GET /api/books?search=`, SERVER mode).
enum SearchBooks: Equatable {
    case idle
    case loading
    case found([TodayBook])
    /// a server without books (LOCAL mode)
    case unavailable
    case failed(String)

    static func load(_ query: String, api: APIClient?) async -> SearchBooks {
        let q = query.trimmingCharacters(in: .whitespaces)
        guard !q.isEmpty else { return .idle }
        guard let api else { return .failed("Not connected.") }
        do {
            return .found(try await api.searchBooks(q))
        } catch APIError.notFound {
            return .unavailable
        } catch {
            return .failed(error.localizedDescription)
        }
    }
}

/// Ask's progress: idle, waiting on the model, an answer, or nothing to go on.
enum AskState: Equatable {
    case idle
    case asking(String)
    case answer(AskResult)
    /// nothing in your docs matched the question
    case nothing(String)
    case failed(String)

    static func run(_ question: String, api: APIClient) async -> AskState {
        do {
            let a = try await api.ask(question)
            guard let id = a.docID else { return .nothing(question) }
            let md = (try? await api.docMarkdown(id)) ?? ""
            return .answer(AskResult(question: question, docID: id, sources: a.sources, docs: a.docs, text: AskAnswerText.parse(md)))
        } catch {
            return .failed(ReviewCopy.failure(error))
        }
    }

    static func reload(_ a: AskResult, api: APIClient) async -> AskResult? {
        guard let md = try? await api.docMarkdown(a.docID) else { return nil }
        var next = a
        next.text = AskAnswerText.parse(md)
        return next
    }
}

struct AskResult: Equatable {
    var question: String
    var docID: DocID
    var sources: Int
    var docs: Int
    var text: AskAnswerText

    /// the synthesis is still being written
    var pending: Bool { text.pending }
}

struct SearchContent: View {
    let query: String
    @Binding var scope: SearchScope
    /// nil = no query yet
    let state: SearchState?
    @Binding var tag: String?
    /// workspaces: the "This workspace · Everywhere" chip (nil = no workspaces)
    var everywhere: Binding<Bool>?
    var books: SearchBooks = .idle
    var ask: AskState = .idle
    var onOpen: (DocID) -> Void = { _ in }
    var onOpenBook: (String) -> Void = { _ in }
    var onAsk: () -> Void = {}

    private var trimmed: String { query.trimmingCharacters(in: .whitespaces) }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                Text("Search")
                    .lapisFont(.display)
                    .foregroundStyle(Color.lapis.ink)
                    .accessibilityAddTraits(.isHeader)
                    .padding(.horizontal, 4)
                Picker("Search in", selection: $scope) {
                    ForEach(SearchScope.allCases, id: \.self) { s in
                        if s == .ask { Label(s.title, systemImage: "sparkle").tag(s) } else { Text(s.title).tag(s) }
                    }
                }
                .pickerStyle(.segmented)
                .sensoryFeedback(.selection, trigger: scope)
                .accessibilityIdentifier("search.scope")
                answerCard
                switch scope {
                case .docs: docs
                case .books: booksSection
                case .ask: if case .idle = ask { askPrompt }
                }
            }
            .padding(.horizontal, 16)
            .padding(.top, 4)
            .padding(.bottom, 32)
            .frame(maxWidth: Lapis.Layout.readingWidth)
            .frame(maxWidth: .infinity)
        }
        .scrollDismissesKeyboard(.interactively)
        .groundBackground()
        .navigationTitle("Search")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar { ToolbarItem(placement: .principal) { Color.clear.frame(width: 1, height: 1).accessibilityHidden(true) } }
    }

    // MARK: docs

    @ViewBuilder private var docs: some View {
        if let state, !state.results.isEmpty {
            chips(state)
            let shown = state.filtered(by: tag)
            VStack(alignment: .leading, spacing: 8) {
                HStack(spacing: 6) {
                    Text("Docs").foregroundStyle(Color.lapis.ink)
                    Text("\(shown.count)").foregroundStyle(Color.lapis.ink3).monospacedDigit()
                    Spacer()
                    if state.offline {
                        Label("Offline · this device", systemImage: "wifi.slash").font(.caption).foregroundStyle(Color.lapis.yellow)
                    }
                }
                .font(.headline)
                .padding(.horizontal, 4)
                .accessibilityElement(children: .combine)
                .accessibilityAddTraits(.isHeader)
                GroupedRows(items: shown, separatorInset: 16) { r in
                    Button { onOpen(r.docID) } label: { SearchResultRow(result: r, query: query) }
                        .buttonStyle(.plain)
                }
            }
        } else if let state {
            if everywhere != nil { chips(state) }
            EmptyCard(icon: "magnifyingglass", title: "No matches for \u{201C}\(query)\u{201D}",
                      hint: state.offline ? "You're offline: only docs on this device were searched." : "Try fewer words, or ask: Taisce answers from your docs.")
        } else {
            EmptyCard(icon: "text.magnifyingglass", title: "Search your library", hint: "Titles and text across every doc. Works offline over the docs on this device.")
        }
    }

    private func chips(_ state: SearchState) -> some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                if let everywhere {
                    FilterChip(title: "This workspace", selected: !everywhere.wrappedValue) { everywhere.wrappedValue = false }
                    FilterChip(title: "Everywhere", selected: everywhere.wrappedValue) { everywhere.wrappedValue = true }
                    if !state.tags.isEmpty { Divider().frame(height: 18) }
                }
                if !state.tags.isEmpty {
                    FilterChip(title: "All", selected: tag == nil) { tag = nil }
                    ForEach(state.tags, id: \.self) { t in
                        FilterChip(title: "#\(t)", selected: tag == t) { tag = tag == t ? nil : t }
                    }
                }
            }
            .padding(.horizontal, 4)
        }
        .scrollClipDisabled()
    }

    // MARK: books

    @ViewBuilder private var booksSection: some View {
        switch books {
        case .idle:
            EmptyCard(icon: "books.vertical", title: "Search your books", hint: "Title, author or series.")
        case .loading:
            SkeletonBlock(height: 120, radius: Lapis.Radius.tab)
        case .unavailable:
            EmptyCard(icon: "books.vertical", title: "No books on this server", hint: "Books live on your Taisce server; this one keeps none.")
        case .failed(let why):
            EmptyCard(icon: "exclamationmark.triangle", title: "Couldn't search books", hint: why)
        case .found(let list):
            if list.isEmpty {
                EmptyCard(icon: "books.vertical", title: "No books match \u{201C}\(trimmed)\u{201D}", hint: "Try the author's surname, or part of the title.")
            } else {
                VStack(alignment: .leading, spacing: 8) {
                    HStack(spacing: 6) {
                        Text("Books").foregroundStyle(Color.lapis.ink)
                        Text("\(list.count)").foregroundStyle(Color.lapis.ink3).monospacedDigit()
                    }
                    .font(.headline)
                    .padding(.horizontal, 4)
                    .accessibilityElement(children: .combine)
                    .accessibilityAddTraits(.isHeader)
                    GroupedRows(items: list, separatorInset: 72) { b in
                        Button { onOpenBook(b.id) } label: {
                            HStack(spacing: 14) {
                                BookCover(id: b.id, title: b.title, author: b.author.isEmpty ? nil : b.author).frame(width: 40)
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(b.title).lapisFont(.cardTitle).foregroundStyle(Color.lapis.ink).lineLimit(2)
                                    let byline = ContinueReading.byline(b)
                                    if !byline.isEmpty { Text(byline).font(.footnote).foregroundStyle(Color.lapis.ink3).lineLimit(1) }
                                }
                                .frame(maxWidth: .infinity, alignment: .leading)
                            }
                            .padding(.horizontal, 16)
                            .padding(.vertical, 10)
                            .contentShape(.rect)
                        }
                        .buttonStyle(.plain)
                        .accessibilityElement(children: .combine)
                    }
                }
            }
        }
    }

    // MARK: ask

    private var askPrompt: some View {
        VStack(alignment: .leading, spacing: 10) {
            Label("Ask your docs", systemImage: "sparkle")
                .font(.footnote.weight(.semibold))
                .foregroundStyle(Color.lapis.agent)
            Text(trimmed.isEmpty ? "Type a question below. Taisce answers only from your docs and says which ones." : "\u{201C}\(trimmed)\u{201D}")
                .lapisFont(.docBody)
                .foregroundStyle(trimmed.isEmpty ? Color.lapis.ink3 : Color.lapis.ink)
            if !trimmed.isEmpty {
                Button("Ask", systemImage: "sparkle", action: onAsk)
                    .buttonStyle(.lapis(.primary))
                    .accessibilityIdentifier("search.ask")
            }
        }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.lapis.card, in: .rect(cornerRadius: Lapis.Radius.tab, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Lapis.Radius.tab, style: .continuous).strokeBorder(Color.lapis.agentSoft, lineWidth: 1))
    }

    @ViewBuilder private var answerCard: some View {
        switch ask {
        case .idle:
            EmptyView()
        case .asking(let q):
            AnswerCard(heading: "Reading your docs\u{2026}", busy: true) {
                Text("\u{201C}\(q)\u{201D}").lapisFont(.docBody).foregroundStyle(Color.lapis.ink3)
                SkeletonLines(count: 3)
            }
        case .nothing(let q):
            AnswerCard(heading: "No answer in your docs") {
                Text("Nothing you've written speaks to \u{201C}\(q)\u{201D}, so no answer was made.").font(.subheadline).foregroundStyle(Color.lapis.ink2)
            }
        case .failed(let why):
            AnswerCard(heading: "Couldn't ask") {
                Text(why).font(.subheadline).foregroundStyle(Color.lapis.ink2)
                Button("Try again", action: onAsk).buttonStyle(.lapis(.secondary, size: .small))
            }
        case .answer(let a):
            AnswerCard(heading: "Answer from your docs", busy: a.pending) {
                if let body = a.text.body {
                    Text(InlineMarkdown.attributed(body))
                        .lapisFont(.docBody)
                        .foregroundStyle(Color.lapis.ink)
                        .textSelection(.enabled)
                } else {
                    Text(a.pending ? "Writing the answer from \(a.sources) passage\(a.sources == 1 ? "" : "s")\u{2026}" : "The passages it found are in the answer doc.")
                        .font(.subheadline).foregroundStyle(Color.lapis.ink3)
                }
                if !a.text.sources.isEmpty {
                    FlowRow(spacing: 8) {
                        ForEach(Array(a.text.sources.enumerated()), id: \.offset) { i, title in
                            CitationChip(n: i + 1, title: title)
                        }
                    }
                }
                Button { onOpen(a.docID) } label: {
                    Label("Open the answer", systemImage: "doc.text")
                }
                .buttonStyle(.lapis(.ghost, size: .small))
            }
        }
    }
}

/// The agent-violet card an answer sits in.
private struct AnswerCard<Content: View>: View {
    let heading: String
    var busy = false
    @ViewBuilder var content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                Image(systemName: "sparkle").accessibilityHidden(true)
                Text(heading)
                if busy { ProgressView().controlSize(.mini).tint(Color.lapis.agent) }
            }
            .font(.footnote.weight(.semibold))
            .foregroundStyle(Color.lapis.agent)
            .accessibilityAddTraits(.isHeader)
            content
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.lapis.card, in: .rect(cornerRadius: Lapis.Radius.tab, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Lapis.Radius.tab, style: .continuous).strokeBorder(Color.lapis.agentSoft, lineWidth: 1))
        .accessibilityElement(children: .contain)
    }
}

/// "1 Review Gate": a doc an answer rests on.
private struct CitationChip: View {
    let n: Int
    let title: String

    var body: some View {
        HStack(spacing: 6) {
            Text("\(n)").lapisFont(.cardTitle).monospacedDigit()
            Text(title).font(.subheadline).lineLimit(1)
        }
        .foregroundStyle(Color.lapis.accent)
        .padding(.horizontal, 12)
        .frame(minHeight: 32)
        .background(Color.lapis.accentSoft, in: .capsule)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Source \(n), \(title)")
    }
}

/// A doc hit: serif title, where it lives, the matching words marked.
struct SearchResultRow: View {
    let result: SearchResult
    let query: String

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(result.title).lapisFont(.cardTitle).foregroundStyle(Color.lapis.ink).lineLimit(2)
                Spacer(minLength: 8)
                if let crumb = result.breadcrumb {
                    Text(crumb).font(.footnote).foregroundStyle(Color.lapis.ink3).lineLimit(1)
                }
            }
            Text(highlighted).font(.subheadline).foregroundStyle(Color.lapis.ink2).lineLimit(3)
            if !result.tags.isEmpty {
                HStack(spacing: 6) { ForEach(result.tags.prefix(3), id: \.self) { TagChip(tag: $0) } }.padding(.top, 2)
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .hoverWash(radius: 0)
        .contentShape(.rect)
        .accessibilityElement(children: .combine)
    }

    var highlighted: AttributedString {
        var s = AttributedString(result.snippet)
        for r in SearchState.matches(in: result.snippet, query: query) {
            guard let lo = AttributedString.Index(r.lowerBound, within: s), let hi = AttributedString.Index(r.upperBound, within: s) else { continue }
            s[lo..<hi].backgroundColor = Color.lapis.mark
            s[lo..<hi].foregroundColor = Color.lapis.ink
        }
        return s
    }
}

#if DEBUG
#Preview("Search") {
    @Previewable @State var scope = SearchScope.docs
    @Previewable @State var tag: String?
    NavigationStack {
        SearchContent(query: "sync", scope: $scope, state: PreviewData.search, tag: $tag,
                      ask: .answer(AskResult(question: "sync", docID: "g-arch", sources: 3, docs: 2,
                                             text: AskAnswerText(body: "The client follows a **global change cursor** and pages from its last seq.", sources: ["Sync contract", "Architecture"], pending: false))))
    }
    .preferredColorScheme(.light)
}

#Preview("Search, dark") {
    @Previewable @State var scope = SearchScope.docs
    @Previewable @State var tag: String? = "ios"
    NavigationStack { SearchContent(query: "sync", scope: $scope, state: PreviewData.search, tag: $tag) }
        .preferredColorScheme(.dark)
}

#Preview("Search, ask") {
    @Previewable @State var scope = SearchScope.ask
    @Previewable @State var tag: String?
    NavigationStack { SearchContent(query: "how does the gate pick a verdict", scope: $scope, state: nil, tag: $tag) }
        .preferredColorScheme(.light)
}
#endif
