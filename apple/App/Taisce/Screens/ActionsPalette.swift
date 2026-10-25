import SwiftUI
import TaisceKit

/// ⌘K: every menu-bar action by name, filtered as you type, Return runs the
/// first (the web's Actions palette). Disabled actions don't show.
struct ActionsPalette: View {
    @Environment(AppModel.self) private var model
    @Environment(Router.self) private var router
    @Environment(\.dismiss) private var dismiss
    @State private var query = ""
    @FocusState private var focused: Bool

    struct Action: Identifiable, Hashable {
        let title: String
        let symbol: String
        let command: AppCommand
        var keys: String?
        var id: String { title }
    }

    static let all: [Action] = [
        Action(title: "New Doc", symbol: "square.and.pencil", command: .newDoc, keys: "\u{2318}N"),
        Action(title: "New Doc Inside This One", symbol: "doc.badge.plus", command: .newDocHere, keys: "\u{2325}\u{2318}N"),
        Action(title: "New Folder", symbol: "folder.badge.plus", command: .newFolder, keys: "\u{21E7}\u{2318}N"),
        Action(title: "Open Doc\u{2026}", symbol: "doc.text.magnifyingglass", command: .open, keys: "\u{2318}O"),
        Action(title: "Search", symbol: "magnifyingglass", command: .search, keys: "\u{21E7}\u{2318}F"),
        Action(title: "Find in Doc", symbol: "text.magnifyingglass", command: .findInDoc, keys: "\u{2318}F"),
        Action(title: "Today", symbol: "house", command: .today),
        Action(title: "To-dos", symbol: "checkmark.circle", command: .todos),
        Action(title: "Review", symbol: "flag", command: .review),
        Action(title: "Books", symbol: "books.vertical", command: .books),
        Action(title: "Add EPUBs\u{2026}", symbol: "plus", command: .addBooks, keys: "\u{21E7}\u{2318}I"),
        Action(title: "Share Link\u{2026}", symbol: "link", command: .shareLink),
        Action(title: "Export as PDF\u{2026}", symbol: "doc.richtext", command: .exportPDF, keys: "\u{2325}\u{2318}E"),
        Action(title: "Show or Hide Sidebar", symbol: "sidebar.left", command: .toggleSidebar, keys: "\u{2318}\\"),
        Action(title: "Refresh", symbol: "arrow.clockwise", command: .refresh, keys: "\u{2318}R"),
        Action(title: "Settings\u{2026}", symbol: "gearshape", command: .settings, keys: "\u{2318},"),
    ]

    /// The actions that apply now, filtered by words in any order.
    static func matching(_ query: String, in actions: [Action] = all, enabled: (AppCommand) -> Bool) -> [Action] {
        let words = query.lowercased().split(separator: " ").map(String.init)
        return actions.filter { a in enabled(a.command) && words.allSatisfy { a.title.lowercased().contains($0) } }
    }

    private var shown: [Action] {
        Self.matching(query) { c in
            router.canPerform(c, signedIn: !model.needsSignIn, picker: model.workspacePicker,
                              canEdit: { model.access(for: $0).canEdit }, canCreate: model.currentAccess.canCreate,
                              shares: model.shareLinks.isAvailable)
        }
    }

    var body: some View {
        NavigationStack {
            List {
                ForEach(shown) { a in
                    Button { run(a.command) } label: {
                        HStack(spacing: 12) {
                            Image(systemName: a.symbol).foregroundStyle(Color.lapis.accent).frame(width: 24).accessibilityHidden(true)
                            Text(a.title).foregroundStyle(Color.lapis.ink)
                            Spacer()
                            if let keys = a.keys {
                                Text(keys).font(.footnote).foregroundStyle(Color.lapis.ink3).accessibilityHidden(true)
                            }
                        }
                        .frame(minHeight: Lapis.Layout.sidebarRow)
                        .contentShape(.rect)
                    }
                    .buttonStyle(.plain)
                    .listRowBackground(Color.lapis.card)
                }
                if shown.isEmpty {
                    Text("No action matches \u{201C}\(query)\u{201D}").font(.subheadline).foregroundStyle(Color.lapis.ink3)
                        .listRowBackground(Color.clear)
                }
            }
            .scrollContentBackground(.hidden)
            .groundBackground()
            .searchable(text: $query, placement: .navigationBarDrawer(displayMode: .always), prompt: "Actions")
            .searchFocused($focused)
            .onSubmit(of: .search) { if let first = shown.first { run(first.command) } }
            .navigationTitle("Actions")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Close") { dismiss() } } }
            .onAppear { focused = true }
        }
        .presentationDetents([.medium, .large])
        .accessibilityIdentifier("actions.palette")
    }

    private func run(_ command: AppCommand) {
        dismiss()
        // after the sheet goes: a command may present its own
        Task { @MainActor in
            try? await Task.sleep(for: .milliseconds(350))
            model.apply(router.perform(command, picker: model.workspacePicker))
        }
    }
}

/// The Mac bar's "Search or ask ⇧⌘F" (Mac-Main): a real field (the Mac's
/// bar draws a Button as its icon alone); typing opens Search on what you
/// typed, Return asks.
struct ToolbarSearchField: View {
    @Environment(Router.self) private var router

    var body: some View {
        @Bindable var router = router
        HStack(spacing: 6) {
            Image(systemName: "magnifyingglass").foregroundStyle(Color.lapis.ink3).accessibilityHidden(true)
            TextField("Search or ask", text: $router.searchQuery)
                .textFieldStyle(.plain)
                .onSubmit { router.showSearch() }
                .onChange(of: router.searchQuery) { _, q in
                    if !q.isEmpty, router.sidebarSelection != .search { router.showSearch() }
                }
            Text("\u{21E7}\u{2318}F").font(.caption).foregroundStyle(Color.lapis.ink3).accessibilityHidden(true)
        }
        .font(.subheadline)
        .padding(.horizontal, 10)
        .frame(width: 240, height: 28)
        .background(Color.lapis.card, in: .capsule)
        .overlay(Capsule().strokeBorder(Color.lapis.line2, lineWidth: 1))
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Search or ask")
        .accessibilityIdentifier("toolbar.search")
    }
}
