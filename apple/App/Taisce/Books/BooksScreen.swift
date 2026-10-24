import SwiftUI
import TaisceKit
import UniformTypeIdentifiers

/// The library (iPhone-Books, iPad-Books, Mac-Books): Continue reading,
/// shelf and series chips, every cover by series then title. iPhone: a
/// cover opens its details as a sheet. iPad and Mac (wide): the details
/// sit in a pane beside the grid, and EPUBs dropped anywhere are added.
struct BooksScreen: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horizontalSizeClass) private var sizeClass
    @State private var filter: BookShelfFilter = .all
    @State private var query = ""
    @State private var width: CGFloat = 400
    /// the book in the pane (wide)
    @State private var selected: String?
    /// the book in the sheet (narrow)
    @State private var detail: ReadingID?
    @State private var reading: ReadingID?
    @State private var importing = false
    @State private var uploading = false
    @State private var dropping = false
    @State private var toast: ToastMessage?
    @State private var removing: Book?
    @State private var coverFor: Book?

    private var store: BookStore { model.books }
    private var shelf: BookShelf { store.shelf }
    /// grid and pane side by side
    private var wide: Bool { sizeClass == .regular && width >= 860 }
    private var regular: Bool { sizeClass == .regular }

    var body: some View {
        HStack(spacing: 0) {
            main
            if wide, store.availability == .available, !shelf.books.isEmpty {
                Divider().ignoresSafeArea()
                pane.frame(width: min(400, max(320, width * 0.3)))
                    .transition(.move(edge: .trailing))
            }
        }
        .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { width = $0 }
        .groundBackground()
        .navigationTitle("Books")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar { toolbar }
        // iPhone: the Search tab finds books; the artboard's shelf has no field
        .modifier(SearchIfRegular(regular: regular, query: $query))
        .refreshable { await store.load(force: true) }
        .task { await store.load() }
        .environment(\.coverImageLoader, store.coverLoader)
        .lapisToast($toast)
        .fileImporter(isPresented: $importing, allowedContentTypes: [Self.epubType], allowsMultipleSelection: true) { result in
            if case let .success(urls) = result { Task { await upload(urls) } }
        }
        .dropDestination(for: URL.self) { urls, _ in
            let epubs = urls.filter { $0.pathExtension.lowercased() == "epub" }
            guard !epubs.isEmpty else { return false }
            Task { await upload(epubs) }
            return true
        } isTargeted: { dropping = $0 }
        .sheet(item: $detail) { d in
            BookDetail(id: d.id, style: .sheet) { id in
                detail = nil
                Task {
                    try? await Task.sleep(for: .milliseconds(350))
                    reading = ReadingID(id: id)
                }
            }
            .environment(\.coverImageLoader, store.coverLoader)
            .presentationDragIndicator(.visible)
        }
        .fullScreenCover(item: $reading) { r in BookReaderScreen(bookID: r.id) }
        .sheet(item: $coverFor) { b in CoverFinder(book: b) }
        .confirmationDialog(removing.map { "Remove \u{201C}\($0.title)\u{201D} from your library?" } ?? "", isPresented: Binding(get: { removing != nil }, set: { if !$0 { removing = nil } }), titleVisibility: .visible, presenting: removing) { b in
            Button("Remove from Library", role: .destructive) { Task { await remove(b) } }
        } message: { _ in
            Text("E-readers stop seeing it at their next sync. The file stays recoverable on the server for 30 days.")
        }
        .onChange(of: shelf.books.map(\.id)) { _, ids in
            if let s = selected, !ids.contains(s) { selected = nil }
            if wide, selected == nil { selected = shelf.continueReading.first?.book.id ?? shown.first?.id }
        }
        .onChange(of: wide) { _, isWide in
            if isWide, selected == nil { selected = shelf.continueReading.first?.book.id ?? shown.first?.id }
        }
    }

    static let epubType = UTType("org.idpf.epub-container") ?? UTType(filenameExtension: "epub") ?? .data

    private var shown: [Book] { shelf.shown(filter, query: query) }

    // MARK: main column

    @ViewBuilder private var main: some View {
        switch (store.availability, shelf.books.isEmpty) {
        case (.unavailable, _):
            EmptyState(symbol: "books.vertical", title: "Books live on your Taisce server",
                       message: "This server keeps no library. Sign in to your own Taisce server to see your books here.")
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        case (.unknown, true) where store.loadError == nil, (.available, true) where store.isLoading:
            loadingSkeleton
        case (_, true) where store.loadError != nil:
            EmptyState(symbol: "wifi.exclamationmark", title: "Your books didn\u{2019}t load", message: store.loadError,
                       actionTitle: "Try Again") { Task { await store.load(force: true) } }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        case (_, true):
            EmptyState(symbol: "books.vertical", title: "No books yet",
                       message: "Add EPUBs from Files. Your e-readers get them from your catalog at their next sync.",
                       actionTitle: "Add EPUBs") { importing = true }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .overlay { if uploading { uploadingCard } }
        default:
            library
        }
    }

    private var library: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: Lapis.Space.s5, pinnedViews: []) {
                header
                if store.isStale, let error = store.loadError {
                    LapisBanner(text: staleText(error), tone: .yellow, symbol: "icloud.slash", actionTitle: "Retry", action: { Task { await store.load(force: true) } })
                }
                if uploading { uploadingCard }
                if filter == .all, query.isEmpty, !shelf.continueReading.isEmpty {
                    continueReading
                }
                chips
                if regular {
                    HStack(alignment: .firstTextBaseline, spacing: Lapis.Space.s2) {
                        Text(query.isEmpty ? filter.title : "Results").font(.title2.weight(.semibold)).foregroundStyle(Color.lapis.ink)
                            .accessibilityAddTraits(.isHeader)
                        Text("by series, then title").font(.subheadline).foregroundStyle(Color.lapis.ink3)
                    }
                    .padding(.top, Lapis.Space.s1)
                }
                grid
            }
            .padding(.horizontal, Lapis.Layout.gutter)
            .padding(.bottom, Lapis.Space.s10)
        }
        .scrollDismissesKeyboard(.immediately)
        .overlay {
            if dropping {
                RoundedRectangle(cornerRadius: Lapis.Radius.card, style: .continuous)
                    .strokeBorder(Color.lapis.accent, style: StrokeStyle(lineWidth: 3, dash: [10, 6]))
                    .background(Color.lapis.accentSoft, in: .rect(cornerRadius: Lapis.Radius.card))
                    .overlay { Label("Drop EPUBs to add them", systemImage: "plus").font(.headline).foregroundStyle(Color.lapis.accent) }
                    .padding(Lapis.Space.s3)
                    .allowsHitTesting(false)
            }
        }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text("Books")
                .lapisFont(.display)
                .foregroundStyle(Color.lapis.ink)
                .accessibilityAddTraits(.isHeader)
            Text(regular ? shelf.summary + " · drop EPUBs from Files anywhere here" : shelf.summary)
                .font(.subheadline)
                .foregroundStyle(Color.lapis.ink3)
                .accessibilityIdentifier("books.summary")
        }
        .padding(.horizontal, 2)
        .padding(.top, Lapis.Space.s2)
    }

    private func staleText(_ error: String) -> String {
        guard let at = store.savedAt else { return error }
        return "Showing your books from \(BookFormat.ago(at)). \(error)"
    }

    private var uploadingCard: some View {
        HStack(spacing: Lapis.Space.s3) {
            ProgressView()
            Text("Adding books\u{2026}").font(.subheadline).foregroundStyle(Color.lapis.ink2)
        }
        .padding(Lapis.Space.s4)
        .frame(maxWidth: .infinity, alignment: .leading)
        .card()
        .accessibilityElement(children: .combine)
    }

    // MARK: continue reading

    @ViewBuilder private var continueReading: some View {
        let items = shelf.continueReading
        if regular {
            LazyVGrid(columns: [GridItem(.adaptive(minimum: 320), spacing: Lapis.Space.s4)], spacing: Lapis.Space.s4) {
                ForEach(items, id: \.book.id) { item in
                    ReadingCard(book: item.book, progress: item.progress, coverURL: store.coverURL(item.book),
                                compact: false, selected: wide && selected == item.book.id,
                                onContinue: { reading = ReadingID(id: item.book.id) },
                                onOpen: { open(item.book) })
                        .contextMenu { menu(item.book) }
                }
            }
            .accessibilityElement(children: .contain)
            .accessibilityLabel("Currently reading")
        } else {
            ScrollView(.horizontal) {
                LazyHStack(spacing: Lapis.Space.s3) {
                    ForEach(items, id: \.book.id) { item in
                        ReadingCard(book: item.book, progress: item.progress, coverURL: store.coverURL(item.book), compact: true,
                                    onContinue: { reading = ReadingID(id: item.book.id) },
                                    onOpen: { open(item.book) })
                            .containerRelativeFrame(.horizontal) { w, _ in items.count == 1 ? w : w * 0.86 }
                            .contextMenu { menu(item.book) }
                    }
                }
                .scrollTargetLayout()
            }
            .scrollTargetBehavior(.viewAligned)
            .scrollIndicators(.hidden)
            .scrollClipDisabled()
            .accessibilityElement(children: .contain)
            .accessibilityLabel("Currently reading")
        }
    }

    // MARK: chips and grid

    private var chips: some View {
        ScrollView(.horizontal) {
            HStack(spacing: Lapis.Space.s2) {
                ShelfChip(title: "All", count: shelf.books.count, selected: filter == .all) { filter = .all }
                if case .series = filter {} else if filter != .all {
                    ShelfChip(title: filter.title, count: shelf.count(filter), selected: true) { filter = .all }
                }
                ForEach(shelf.series.prefix(8), id: \.name) { s in
                    ShelfChip(title: s.name, count: s.count, selected: filter == .series(s.name)) {
                        filter = filter == .series(s.name) ? .all : .series(s.name)
                    }
                }
                if case let .series(name) = filter, !shelf.series.prefix(8).contains(where: { $0.name == name }) {
                    ShelfChip(title: name, count: shelf.count(filter), selected: true) { filter = .all }
                }
            }
            .padding(.horizontal, 2)
        }
        .scrollIndicators(.hidden)
        .scrollClipDisabled()
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Shelves")
    }

    @ViewBuilder private var grid: some View {
        let books = shown
        if books.isEmpty {
            EmptyCard(icon: "magnifyingglass", title: query.isEmpty ? "Nothing on this shelf" : "No books match \u{201C}\(query)\u{201D}",
                      hint: query.isEmpty ? "Choose another shelf above." : "Try a title, an author or a series.")
        } else {
            LazyVGrid(columns: [GridItem(.adaptive(minimum: regular ? 140 : 100, maximum: 210), spacing: regular ? Lapis.Space.s5 : Lapis.Space.s4, alignment: .top)],
                      spacing: Lapis.Space.s6) {
                ForEach(books) { b in
                    Button { open(b) } label: {
                        BookTile(book: b, state: shelf.stateLine(b), isNew: shelf.isNew(b), selected: wide && selected == b.id, coverURL: store.coverURL(b))
                    }
                    .buttonStyle(CoverPressStyle())
                    .contextMenu { menu(b) } preview: {
                        BookCover(id: b.id, title: b.title, author: b.authorName, imageURL: store.coverURL(b))
                            .frame(width: 220)
                            .environment(\.coverImageLoader, store.coverLoader)
                    }
                    .accessibilityAction(named: "Read") { reading = ReadingID(id: b.id) }
                    .accessibilityIdentifier("book.\(b.id)")
                }
            }
        }
    }

    @ViewBuilder private func menu(_ b: Book) -> some View {
        let started = shelf.status(b.id) == .reading
        Button { reading = ReadingID(id: b.id) } label: { Label(started ? "Continue Reading" : "Read", systemImage: "book") }
        Button { open(b) } label: { Label("Details", systemImage: "info.circle") }
        Button { Task { await share(b) } } label: { Label("Send to\u{2026}", systemImage: "square.and.arrow.up") }
        Button { coverFor = b } label: { Label("Find Colour Cover\u{2026}", systemImage: "photo") }
        if let s = b.seriesName {
            Button { filter = .series(s) } label: { Label("Show \(s)", systemImage: "books.vertical") }
        }
        Divider()
        Button(role: .destructive) { removing = b } label: { Label("Remove from Library\u{2026}", systemImage: "trash") }
    }

    // MARK: pane (wide)

    @ViewBuilder private var pane: some View {
        if let id = selected, store.book(id) != nil {
            BookDetail(id: id, style: .pane) { reading = ReadingID(id: $0) }
                .id(id)
        } else {
            EmptyState(symbol: "book.closed", title: "Choose a book", message: "Its details and cover show here.")
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .background(Color.lapis.bg)
        }
    }

    // MARK: toolbar

    @ToolbarContentBuilder private var toolbar: some ToolbarContent {
        ToolbarItem(placement: .principal) { Color.clear.frame(width: 1, height: 1).accessibilityHidden(true) }
        ToolbarItemGroup(placement: .topBarTrailing) {
            Menu {
                Picker("Shelf", selection: $filter) {
                    Label("All books", systemImage: "books.vertical").tag(BookShelfFilter.all)
                    ForEach(BookShelfFilter.shelves, id: \.self) { f in
                        Text("\(f.title)  \(shelf.count(f))").tag(f)
                    }
                }
                .pickerStyle(.inline)
                if !shelf.series.isEmpty {
                    Picker("Series", selection: $filter) {
                        ForEach(shelf.series, id: \.name) { s in
                            Text("\(s.name)  \(s.count)").tag(BookShelfFilter.series(s.name))
                        }
                    }
                    .pickerStyle(.menu)
                }
            } label: {
                Image(systemName: filter == .all ? "line.3.horizontal.decrease" : "line.3.horizontal.decrease.circle.fill")
            }
            .accessibilityLabel("Shelves")
            .accessibilityValue(filter.title)
            .accessibilityIdentifier("books.filter")
            Button { importing = true } label: {
                if regular { Label("Add EPUBs", systemImage: "plus").labelStyle(.titleAndIcon) } else { Image(systemName: "plus") }
            }
            .accessibilityLabel("Add EPUBs")
            .accessibilityIdentifier("books.add")
            .keyboardShortcut("i", modifiers: [.command, .shift])
            .disabled(store.availability == .unavailable || uploading)
        }
    }

    // MARK: actions

    private func open(_ b: Book) {
        if wide {
            withAnimation(Lapis.Motion.fast) { selected = b.id }
        } else {
            detail = ReadingID(id: b.id)
        }
    }

    private func upload(_ urls: [URL]) async {
        uploading = true
        defer { uploading = false }
        do {
            let results = try await store.upload(urls)
            toast = BookUploadSummary.toast(results)
            if let first = results.first(where: { $0.status == .added })?.book, wide { selected = first.id }
        } catch {
            toast = ToastMessage(text: BookStore.describe(error), tone: .error)
        }
    }

    private func share(_ b: Book) async {
        do {
            let file = try await store.exportFile(.book(b.id), name: b.downloadFilename)
            ShareSheetPresenter.present([file])
        } catch {
            toast = ToastMessage(text: BookStore.describe(error), tone: .error)
        }
    }

    private func remove(_ b: Book) async {
        do {
            try await store.delete(b.id)
            Haptics.success()
            toast = ToastMessage(text: "Removed \u{201C}\(b.title)\u{201D}", tone: .success)
        } catch {
            toast = ToastMessage(text: BookStore.describe(error), tone: .error)
        }
    }

    private var loadingSkeleton: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: Lapis.Space.s5) {
                header.redacted(reason: .placeholder)
                SkeletonBlock(height: 140)
                LazyVGrid(columns: [GridItem(.adaptive(minimum: regular ? 140 : 100), spacing: Lapis.Space.s4)], spacing: Lapis.Space.s5) {
                    ForEach(0..<9, id: \.self) { _ in
                        VStack(alignment: .leading, spacing: 6) {
                            SkeletonBlock(height: regular ? 210 : 150)
                            SkeletonLines(count: 2)
                        }
                    }
                }
            }
            .padding(.horizontal, Lapis.Layout.gutter)
        }
        .accessibilityLabel("Loading your books")
    }
}

/// The toast after an upload: "Added 3 books · 1 already here".
enum BookUploadSummary {
    static func line(_ results: [BookUploadResult]) -> (text: String, tone: ToastMessage.Tone) {
        let added = results.count { $0.status == .added }
        let updated = results.count { $0.status == .updated }
        let here = results.count { $0.status == .alreadyHere }
        let failed = results.filter { $0.status == .error || $0.status == .clash }
        var parts: [String] = []
        if added > 0 { parts.append(added == 1 ? "Added 1 book" : "Added \(added) books") }
        if updated > 0 { parts.append("\(updated) updated") }
        if here > 0 { parts.append("\(here) already here") }
        if !failed.isEmpty {
            let first = failed[0]
            parts.append(failed.count == 1 ? "\(first.filename): \(first.message ?? "not added")" : "\(failed.count) not added")
        }
        if parts.isEmpty { parts = ["Nothing to add"] }
        return (parts.joined(separator: " · "), failed.isEmpty ? (added + updated > 0 ? .success : .neutral) : .warning)
    }

    static func toast(_ results: [BookUploadResult]) -> ToastMessage {
        let l = line(results)
        return ToastMessage(text: l.text, tone: l.tone)
    }
}

/// A cover presses in a touch (Reduce Motion: no scale, a dim).
struct CoverPressStyle: ButtonStyle {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .scaleEffect(configuration.isPressed && !reduceMotion ? 0.97 : 1)
            .opacity(configuration.isPressed && reduceMotion ? 0.7 : 1)
            .animation(Lapis.Motion.fast, value: configuration.isPressed)
            .hoverEffect(.lift)
    }
}

/// The share sheet from a menu (no view to hang a sheet on).
@MainActor
enum ShareSheetPresenter {
    static func present(_ items: [Any]) {
        guard let scene = UIApplication.shared.connectedScenes.compactMap({ $0 as? UIWindowScene }).first(where: { $0.activationState == .foregroundActive }),
              var top = scene.keyWindow?.rootViewController
        else { return }
        while let next = top.presentedViewController { top = next }
        let vc = UIActivityViewController(activityItems: items, applicationActivities: nil)
        vc.popoverPresentationController?.sourceView = top.view
        vc.popoverPresentationController?.sourceRect = CGRect(x: top.view.bounds.midX, y: top.view.bounds.midY, width: 1, height: 1)
        top.present(vc, animated: true)
    }
}

/// The library's filter field on iPad and the Mac (the toolbar's search).
private struct SearchIfRegular: ViewModifier {
    let regular: Bool
    @Binding var query: String

    func body(content: Content) -> some View {
        if regular {
            content.searchable(text: $query, placement: .toolbar, prompt: "Title, author or series")
        } else {
            content
        }
    }
}
