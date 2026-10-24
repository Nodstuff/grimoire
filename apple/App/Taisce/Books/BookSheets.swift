import SwiftUI
import TaisceKit

/// Colour covers the server found on Open Library for one book: pick one,
/// keep the book's own, or search with different terms.
struct CoverFinder: View {
    let book: Book
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var found: CoverCandidates?
    @State private var loading = true
    @State private var error: String?
    @State private var choosing: String?
    @State private var title = ""
    @State private var author = ""
    @State private var searching = false

    private var store: BookStore { model.books }

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: Lapis.Space.s5) {
                    if loading {
                        LazyVGrid(columns: columns, spacing: Lapis.Space.s4) {
                            ForEach(0..<4, id: \.self) { _ in SkeletonBlock(height: 180) }
                        }
                        .accessibilityLabel("Looking for covers")
                    } else if let error {
                        EmptyCard(icon: "wifi.exclamationmark", title: "Couldn\u{2019}t look", hint: error)
                    } else if let found, found.unavailable {
                        EmptyCard(icon: "clock", title: "Open Library isn\u{2019}t answering", hint: "Try again in a little while.")
                    } else if let found, found.candidates.isEmpty {
                        EmptyCard(icon: "photo", title: "No colour covers found", hint: "Try a different title or author below.")
                    } else if let found {
                        LazyVGrid(columns: columns, spacing: Lapis.Space.s4) {
                            ForEach(found.candidates) { c in candidate(c) }
                        }
                    }
                    searchAgain
                }
                .padding(Lapis.Layout.gutter)
            }
            .groundBackground()
            .navigationTitle("Colour cover")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
            }
            .environment(\.coverImageLoader, store.coverLoader)
        }
        .task { await look(refresh: false) }
    }

    private var columns: [GridItem] { [GridItem(.adaptive(minimum: 130), spacing: Lapis.Space.s4)] }

    private func candidate(_ c: CoverCandidate) -> some View {
        Button {
            Task { await choose(c) }
        } label: {
            VStack(alignment: .leading, spacing: 6) {
                BookCover(id: book.id, title: book.title, author: book.authorName, imageURL: store.api?.candidateURL(c))
                    .clipShape(.rect(cornerRadius: 6))
                    .overlay { if choosing == c.id { ProgressView().padding().background(.regularMaterial, in: .circle) } }
                Text(c.sourceLabel).font(.subheadline.weight(.semibold)).foregroundStyle(Color.lapis.ink)
                Text("\(c.width) \u{00D7} \(c.height)").font(.caption).foregroundStyle(Color.lapis.ink3)
            }
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .disabled(choosing != nil)
        .accessibilityLabel("\(c.sourceLabel) cover, \(c.width) by \(c.height)")
        .accessibilityHint("Uses this cover")
    }

    private var searchAgain: some View {
        VStack(alignment: .leading, spacing: Lapis.Space.s2) {
            SectionLabel("Search with a different title")
            LapisCard {
                VStack(spacing: Lapis.Space.s3) {
                    TextField("Title", text: $title).textInputAutocapitalization(.words)
                    Divider()
                    TextField("Author", text: $author).textInputAutocapitalization(.words)
                    Button {
                        Task { await look(refresh: true, terms: true) }
                    } label: {
                        Label("Search Open Library", systemImage: "magnifyingglass").frame(maxWidth: .infinity)
                    }
                    .buttonStyle(.lapis(.secondary, fullWidth: true))
                    .disabled(title.trimmingCharacters(in: .whitespaces).isEmpty || searching)
                }
            }
        }
        .onAppear {
            if title.isEmpty { title = book.title }
            if author.isEmpty { author = book.authorName ?? "" }
        }
    }

    private func look(refresh: Bool, terms: Bool = false) async {
        guard let api = store.api else { return }
        loading = true
        searching = terms
        error = nil
        defer {
            loading = false
            searching = false
        }
        do {
            if terms {
                found = try await api.findCoverCandidates(book.id, refresh: true, title: title, author: author)
            } else {
                let stored = try await api.coverCandidates(book.id)
                found = stored.lookedUpAt == nil ? try await api.findCoverCandidates(book.id, refresh: refresh, title: nil, author: nil) : stored
            }
        } catch {
            self.error = BookStore.describe(error)
        }
    }

    private func choose(_ c: CoverCandidate) async {
        choosing = c.id
        defer { choosing = nil }
        do {
            _ = try await store.chooseCover(book.id, candidate: c.id)
            Haptics.success()
            dismiss()
        } catch {
            self.error = BookStore.describe(error)
        }
    }
}

/// Title, author, series and number (what the feed and e-readers show).
struct BookEditSheet: View {
    let book: Book
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var title = ""
    @State private var author = ""
    @State private var series = ""
    @State private var index = ""
    @State private var error: String?
    @State private var saving = false

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("Title", text: $title)
                    TextField("Author", text: $author)
                }
                Section {
                    TextField("Series", text: $series)
                    TextField("Number in series", text: $index).keyboardType(.decimalPad)
                } footer: {
                    Text("E-readers show the new details at their next sync.")
                }
                if let error {
                    Section { Label(error, systemImage: "exclamationmark.triangle").foregroundStyle(Color.lapis.red) }
                }
            }
            .scrollContentBackground(.hidden)
            .background(Color.lapis.bg)
            .navigationTitle("Edit Details")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save") { Task { await save() } }.disabled(saving)
                }
            }
            .onAppear {
                title = book.title
                author = book.authorName ?? ""
                series = book.seriesName ?? ""
                index = book.seriesIndex.map(BookFormat.number) ?? ""
            }
        }
    }

    private func save() async {
        do {
            let patch = try BookPatch.edits(from: book, title: title, author: author, series: series, seriesIndex: index)
            if patch.isEmpty {
                dismiss()
                return
            }
            saving = true
            defer { saving = false }
            _ = try await model.books.update(book.id, patch: patch)
            dismiss()
        } catch {
            self.error = (error as? LocalizedError)?.errorDescription ?? BookStore.describe(error)
        }
    }
}
