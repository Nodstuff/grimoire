import SwiftUI
import TaisceKit

/// A book's details (iPhone-Book-Detail, the iPad pane, the Mac inspector):
/// the cover over its own colour, series, title, author, where you are,
/// Continue reading, Open in Books / Send to…, the cover choice, and the
/// file. `.sheet` has a close button; `.pane` sits beside the grid; `.page`
/// is pushed (the shell's `Route.book(id)`).
struct BookDetail: View {
    enum Style { case sheet, pane, page }

    let id: String
    var style: Style = .page
    /// the screen showing this opens the reader itself (else a cover here does)
    var onRead: ((String) -> Void)?

    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var tint: Color?
    @State private var reading: String?
    @State private var share: ShareFile?
    @State private var openIn: ShareFile?
    @State private var preparing = false
    @State private var editing = false
    @State private var findingCover = false
    @State private var confirmRemove = false
    @State private var toast: ToastMessage?
    @Environment(\.dynamicTypeSize) private var typeSize

    private var store: BookStore { model.books }

    var body: some View {
        Group {
            if let book = store.book(id) {
                content(book)
            } else if store.isLoading || store.availability == .unknown {
                ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity).groundBackground()
                    .task { await store.load() }
            } else {
                EmptyState(symbol: "book.closed", title: "Not in your library", message: "This book was removed, or it's on another server.")
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .groundBackground()
            }
        }
        .lapisToast($toast)
        .fullScreenCover(item: Binding(get: { reading.map(ReadingID.init) }, set: { reading = $0?.id })) { r in
            BookReaderScreen(bookID: r.id)
        }
        .sheet(item: $share) { f in ActivitySheet(items: [f.url]).presentationDetents([.medium, .large]) }
        .background { if let openIn { OpenInMenu(file: openIn.url) { self.openIn = nil } } }
    }

    private func content(_ book: Book) -> some View {
        let progress = store.progress(book.id)
        let coverURL = store.coverURL(book)
        return ScrollView {
            VStack(spacing: Lapis.Space.s5) {
                header(book, coverURL: coverURL)
                if let progress, progress.status != .unread {
                    progressBlock(progress)
                }
                actions(book, progress: progress)
                coverSection(book)
                ereaderSection(book, progress: progress)
                fileInfo(book)
            }
            .padding(.horizontal, style == .pane ? Lapis.Space.s5 : Lapis.Space.s5)
            .padding(.bottom, Lapis.Space.s8)
            .frame(maxWidth: Lapis.Layout.readingWidth)
            .frame(maxWidth: .infinity)
        }
        .scrollBounceBehavior(.basedOnSize)
        .background(alignment: .top) { wash }
        .background(Color.lapis.bg)
        .overlay(alignment: .top) { if style == .sheet { sheetBar(book) } }
        .toolbar { if style != .sheet { ToolbarItem(placement: .primaryAction) { moreMenu(book) } } }
        .navigationTitle(style == .page ? book.title : "")
        .navigationBarTitleDisplayMode(.inline)
        .modifier(CoverTintReader(book: book, url: coverURL, tint: $tint))
        .environment(\.coverImageLoader, store.coverLoader)
        .sheet(isPresented: $editing) { BookEditSheet(book: book) }
        .sheet(isPresented: $findingCover) { CoverFinder(book: book) }
        .confirmationDialog("Remove \u{201C}\(book.title)\u{201D} from your library?", isPresented: $confirmRemove, titleVisibility: .visible) {
            Button("Remove from Library", role: .destructive) { Task { await remove(book) } }
        } message: {
            Text("E-readers stop seeing it at their next sync. The file stays recoverable on the server for 30 days.")
        }
    }

    /// The cover's own colour, fading into the ground.
    private var wash: some View {
        LinearGradient(colors: [(tint ?? Color.lapis.hover).opacity(0.55), Color.lapis.bg], startPoint: .top, endPoint: .bottom)
            .frame(height: 520)
            .ignoresSafeArea()
            .accessibilityHidden(true)
    }

    private func sheetBar(_ book: Book) -> some View {
        HStack {
            Button { dismiss() } label: {
                Image(systemName: "xmark").font(.body.weight(.semibold)).frame(width: 40, height: 40)
            }
            .buttonStyle(.glass)
            .buttonBorderShape(.circle)
            .accessibilityLabel("Close")
            Spacer()
            moreMenu(book)
                .buttonStyle(.glass)
                .buttonBorderShape(.circle)
        }
        .padding(.horizontal, Lapis.Space.s4)
        .padding(.top, Lapis.Space.s4)
    }

    private func moreMenu(_ book: Book) -> some View {
        Menu {
            Button { Task { await prepare(book, openIn: false) } } label: { Label("Share EPUB…", systemImage: "square.and.arrow.up") }
            Button { editing = true } label: { Label("Edit Details…", systemImage: "pencil") }
            Button { findingCover = true } label: { Label("Find Colour Cover…", systemImage: "photo") }
            Divider()
            Button(role: .destructive) { confirmRemove = true } label: { Label("Remove from Library…", systemImage: "trash") }
        } label: {
            Image(systemName: "ellipsis").font(.body.weight(.semibold)).frame(width: 40, height: 40)
        }
        .accessibilityLabel("More")
        .accessibilityIdentifier("book.more")
    }

    private func header(_ book: Book, coverURL: URL?) -> some View {
        VStack(spacing: Lapis.Space.s2) {
            BookCover(id: book.id, title: book.title, author: book.authorName, imageURL: coverURL)
                .frame(width: style == .pane ? 150 : 152)
                .lapisShadow(.pop)
                .padding(.top, style == .sheet ? 64 : Lapis.Space.s6)
                .padding(.bottom, Lapis.Space.s4)
                .accessibilityHidden(true)
            if let series = BookFormat.seriesLine(book) {
                Text(series.uppercased())
                    .font(.footnote.weight(.semibold))
                    .tracking(0.8)
                    .foregroundStyle(Color.lapis.ink3)
                    .multilineTextAlignment(.center)
            }
            Text(book.title)
                .lapisFont(style == .pane ? .docHeading1 : .docTitle)
                .foregroundStyle(Color.lapis.ink)
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)
                .accessibilityAddTraits(.isHeader)
            if let author = book.authorName {
                Text(author).font(.title3).foregroundStyle(Color.lapis.ink2).multilineTextAlignment(.center)
            }
        }
        .frame(maxWidth: .infinity)
    }

    private func progressBlock(_ p: BookProgress) -> some View {
        VStack(spacing: 6) {
            ProgressBar(value: p.percentage / 100)
            ViewThatFits(in: .horizontal) {
                HStack {
                    progressText(p)
                    Spacer(minLength: Lapis.Space.s2)
                    Text(BookFormat.deviceLine(p)).foregroundStyle(Color.lapis.ink3)
                }
                VStack(alignment: .leading, spacing: 2) {
                    progressText(p)
                    Text(BookFormat.deviceLine(p)).foregroundStyle(Color.lapis.ink3)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .font(.subheadline)
        }
        .accessibilityElement(children: .combine)
    }

    private func progressText(_ p: BookProgress) -> some View {
        Group {
            if p.status == .finished {
                Text("Finished").fontWeight(.semibold).foregroundStyle(Color.lapis.ink)
            } else {
                Text(BookFormat.percent(p.percentage)).fontWeight(.semibold).foregroundStyle(Color.lapis.ink)
            }
        }
    }

    private func actions(_ book: Book, progress: BookProgress?) -> some View {
        let started = progress.map { $0.status == .reading } ?? false
        let finished = progress?.status == .finished
        return VStack(spacing: Lapis.Space.s3) {
            Button { read(book.id) } label: {
                Label(started ? "Continue reading" : finished ? "Read again" : "Read", systemImage: "book")
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.lapis(.primary, fullWidth: true))
            .controlSize(.large)
            .accessibilityIdentifier("book.read")
            let pair = typeSize.isAccessibilitySize ? AnyLayout(VStackLayout(spacing: Lapis.Space.s3)) : AnyLayout(HStackLayout(spacing: Lapis.Space.s3))
            pair {
                Button { Task { await prepare(book, openIn: true) } } label: {
                    Label("Open in Books", systemImage: "square.and.arrow.up").frame(maxWidth: .infinity)
                }
                .buttonStyle(.lapis(.secondary, fullWidth: true))
                Button { Task { await prepare(book, openIn: false) } } label: {
                    Label("Send to…", systemImage: "paperplane").frame(maxWidth: .infinity)
                }
                .buttonStyle(.lapis(.secondary, fullWidth: true))
                .accessibilityHint("AirDrop, Mail, or Save to Files")
            }
            .disabled(preparing)
            .overlay { if preparing { ProgressView() } }
        }
        // the iPad pane's buttons are words only (artboard); accessibility sizes are the button's own
        .lapisButtonIcons(style != .pane)
    }

    @ViewBuilder private func coverSection(_ book: Book) -> some View {
        VStack(alignment: .leading, spacing: Lapis.Space.s2) {
            SectionLabel("Cover")
            LapisCard {
                let row = typeSize.isAccessibilitySize ? AnyLayout(VStackLayout(alignment: .leading, spacing: Lapis.Space.s3))
                    : AnyLayout(HStackLayout(alignment: .top, spacing: Lapis.Space.s4))
                row {
                    coverChoices(book)
                    VStack(alignment: .leading, spacing: Lapis.Space.s1) {
                        Text(coverHeadline(book)).font(.headline).foregroundStyle(Color.lapis.ink)
                        Text(coverExplanation(book)).font(.subheadline).foregroundStyle(Color.lapis.ink2)
                            .fixedSize(horizontal: false, vertical: true)
                        Button(coverAction(book)) { findingCover = true }
                            .font(.subheadline.weight(.medium))
                            .foregroundStyle(Color.lapis.accent)
                            .frame(minHeight: Lapis.Layout.minTarget)
                            .accessibilityIdentifier("book.findCover")
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
        }
    }

    @ViewBuilder private func coverChoices(_ book: Book) -> some View {
        if book.showsColorCover, book.hasEmbeddedCover == true {
            HStack(spacing: Lapis.Space.s2) {
                coverRadio(title: "Original", selected: false, url: store.coverURL(book, embedded: true), book: book) {
                    Task { await useOwnCover(book) }
                }
                coverRadio(title: "Colour", selected: true, url: store.coverURL(book), book: book) {}
            }
            .accessibilityElement(children: .contain)
            .accessibilityLabel("Cover")
        } else {
            BookCover(id: book.id, title: book.title, author: book.authorName, imageURL: store.coverURL(book))
                .frame(width: 56)
                .clipShape(.rect(cornerRadius: 4))
                .accessibilityHidden(true)
        }
    }

    private func coverRadio(title: String, selected: Bool, url: URL?, book: Book, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            VStack(spacing: 6) {
                BookCover(id: book.id, title: book.title, author: book.authorName, imageURL: url)
                    .frame(width: 56)
                    .clipShape(.rect(cornerRadius: 4))
                    .padding(3)
                    .overlay(RoundedRectangle(cornerRadius: 7).strokeBorder(selected ? Color.lapis.accent : .clear, lineWidth: 2.5))
                Text(title).font(.caption.weight(selected ? .semibold : .regular))
                    .foregroundStyle(selected ? Color.lapis.accent : Color.lapis.ink3)
            }
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .accessibilityLabel(title == "Original" ? "The book's own cover" : "Colour cover from Open Library")
        .accessibilityAddTraits(selected ? [.isSelected] : [])
    }

    private func coverHeadline(_ b: Book) -> String {
        if b.showsColorCover { return "Colour cover in use" }
        if b.wantsCover { return "No cover" }
        if b.wantsColourCover { return "Black-and-white cover" }
        return "The book's own cover"
    }

    private func coverExplanation(_ b: Book) -> String {
        if b.showsColorCover {
            return b.hasEmbeddedCover == true
                ? "The EPUB\u{2019}s own cover is black and white. This one is from Open Library; your e-readers get it at their next sync."
                : "From Open Library; your e-readers get it at their next sync."
        }
        if b.wantsCover { return "The EPUB has no cover. Open Library may have one." }
        if b.wantsColourCover { return "The EPUB\u{2019}s cover is black and white. Open Library may have it in colour." }
        return "The cover inside the EPUB."
    }

    private func coverAction(_ b: Book) -> String {
        if b.showsColorCover { return "Find another\u{2026}" }
        if b.wantsCover { return "Find a cover\u{2026}" }
        return "Find a colour cover\u{2026}"
    }

    @ViewBuilder private func ereaderSection(_ book: Book, progress: BookProgress?) -> some View {
        let n = store.shelf.ereaderCount
        if n > 0 || progress != nil {
            VStack(alignment: .leading, spacing: Lapis.Space.s2) {
                SectionLabel("On your e-readers")
                LapisCard {
                    VStack(alignment: .leading, spacing: Lapis.Space.s2) {
                        if let progress, progress.status != .unread {
                            HStack {
                                Image(systemName: "ipad.landscape").foregroundStyle(Color.lapis.ink3).accessibilityHidden(true)
                                Text(progress.device.isEmpty ? "Another device" : progress.device).foregroundStyle(Color.lapis.ink)
                                Spacer()
                                Text("\(BookFormat.percent(progress.percentage)) · \(progress.updatedAt.map { BookFormat.ago($0) } ?? "")")
                                    .foregroundStyle(Color.lapis.ink3)
                            }
                            .font(.subheadline)
                            .accessibilityElement(children: .combine)
                        }
                        Text(n == 0 ? "No e-readers are set up. Add one in Settings \u{203A} E-readers." :
                            n == 1 ? "Your e-reader gets this book from your catalog at its next sync." :
                            "Your \(n) e-readers get this book from your catalog at their next sync.")
                            .font(.footnote)
                            .foregroundStyle(Color.lapis.ink3)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
            }
        }
    }

    private func fileInfo(_ book: Book) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text("\(book.originalFilename.isEmpty ? book.downloadFilename : book.originalFilename) · \(BookFormat.size(book.size))")
            if let at = book.addedAt {
                Text("Added \(at.formatted(.dateTime.day().month(.abbreviated).year()))")
            }
        }
        .font(.footnote)
        .foregroundStyle(Color.lapis.ink3)
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, Lapis.Space.s1)
        .textSelection(.enabled)
    }

    // MARK: actions

    private func read(_ id: String) {
        if let onRead { onRead(id) } else { reading = id }
    }

    private func prepare(_ book: Book, openIn: Bool) async {
        preparing = true
        defer { preparing = false }
        do {
            let file = try await store.exportFile(.book(book.id), name: book.downloadFilename)
            if openIn { self.openIn = ShareFile(url: file) } else { share = ShareFile(url: file) }
        } catch {
            toast = ToastMessage(text: BookStore.describe(error), tone: .error)
        }
    }

    private func useOwnCover(_ book: Book) async {
        do {
            _ = try await store.chooseCover(book.id, candidate: nil)
            toast = ToastMessage(text: "Showing the book\u{2019}s own cover", tone: .success)
        } catch {
            toast = ToastMessage(text: BookStore.describe(error), tone: .error)
        }
    }

    private func remove(_ book: Book) async {
        do {
            try await store.delete(book.id)
            Haptics.success()
            if style != .pane { dismiss() }
        } catch {
            toast = ToastMessage(text: BookStore.describe(error), tone: .error)
        }
    }
}

/// An identifiable book id for `fullScreenCover(item:)`.
struct ReadingID: Identifiable, Hashable {
    let id: String
}

/// "Open in Books": the system's Open In menu over the EPUB (apps that open
/// EPUBs, Books first), presented from the window's front controller.
struct OpenInMenu: UIViewControllerRepresentable {
    let file: URL
    let done: () -> Void

    func makeCoordinator() -> Coordinator { Coordinator(done: done) }

    func makeUIViewController(context: Context) -> UIViewController {
        let vc = UIViewController()
        vc.view.isUserInteractionEnabled = false
        return vc
    }

    func updateUIViewController(_ vc: UIViewController, context: Context) {
        guard context.coordinator.controller == nil else { return }
        let c = UIDocumentInteractionController(url: file)
        c.uti = "org.idpf.epub-container"
        c.delegate = context.coordinator
        context.coordinator.controller = c
        DispatchQueue.main.async {
            let rect = CGRect(x: vc.view.bounds.midX, y: vc.view.bounds.midY, width: 1, height: 1)
            if !c.presentOpenInMenu(from: rect, in: vc.view, animated: true) {
                // nothing installed that opens EPUBs: the share sheet has Save to Files
                c.presentOptionsMenu(from: rect, in: vc.view, animated: true)
            }
        }
    }

    final class Coordinator: NSObject, UIDocumentInteractionControllerDelegate {
        var controller: UIDocumentInteractionController?
        let done: () -> Void
        init(done: @escaping () -> Void) { self.done = done }
        func documentInteractionControllerDidDismissOpenInMenu(_ c: UIDocumentInteractionController) { done() }
        func documentInteractionControllerDidDismissOptionsMenu(_ c: UIDocumentInteractionController) { done() }
    }
}

