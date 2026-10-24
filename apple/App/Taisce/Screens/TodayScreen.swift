import SwiftUI
import TaisceKit

/// Today: what wants a look (the review queue), the book in progress,
/// what's due and the pinned docs. Due items come from the server's
/// read-only due list (the cache offline); pins are local.
struct TodayScreen: View {
    @Environment(AppModel.self) private var model
    @Environment(Router.self) private var router
    @State private var board: TodoBoard?
    @State private var taps = RingTaps()
    @State private var capturing = false

    var body: some View {
        let home = model.home
        let reading = home.continueReading
        TodayContent(
            dayLine: RelativeTime.dayLine(),
            badge: SyncBadge.make(status: model.syncStatus, pending: model.pendingWrites),
            review: home.review,
            reading: reading,
            readingCover: reading.flatMap { home.covers[$0.id] },
            due: TodoBoard.shown(board, hasSynced: model.hasSynced, status: model.syncStatus).map { TodayDue.rows($0) },
            pinned: pinnedCards,
            onReview: router.showReview,
            onOpen: { router.open(.doc($0)) },
            onRead: { router.readBook($0.id) },
            onBooks: router.showBooks,
            onAllTodos: router.showTodos,
            onUnpin: model.togglePin,
            checked: taps.checked,
            // a viewer's list is read-only (ADR 0004)
            onToggleDone: model.currentAccess.canEdit ? { e in taps.tap(e) { await model.markDone($0) } } : nil
        )
        .safeAreaInset(edge: .top, spacing: 0) { WorkspaceBar() } // workspaces
        .toolbar { toolbar }
        .sheet(isPresented: $capturing) { CaptureTodoSheet() }
        .refreshable {
            try? await model.sync?.catchUp()
            await reload()
            await home.loadReview()
            await home.loadReading()
        }
        .task(id: model.todoRevision) { await reload() }
        // the queue moves with every change the server journals
        .task(id: ReloadKey(cursor: model.cursor, auth: model.authPhase)) {
            try? await Task.sleep(for: .milliseconds(400))
            guard !Task.isCancelled else { return }
            await home.loadReview()
        }
        .task(id: model.authPhase) {
            await home.loadReading()
            await home.loadProfile()
        }
        .task(id: model.pins) {
            for id in model.pins { await model.loadEditMeta(id) }
        }
    }

    private struct ReloadKey: Equatable {
        var cursor: Int
        var auth: AppModel.AuthPhase
    }

    private var pinnedCards: [PinnedCard] {
        DocCardModel.pinned(model.workspacePins, index: model.index, meta: model.editMeta).map { doc in
            let ws = model.index.byID[doc.id]?.workspaceID
            return PinnedCard(
                doc: doc,
                crumb: model.index.breadcrumb(of: doc.id),
                color: ws.flatMap { model.workspacePicker.color(.id($0)) },
                author: model.editMeta[doc.id]?.author
            )
        }
    }

    @ToolbarContentBuilder private var toolbar: some ToolbarContent {
        // the screen's own serif title stands in for the bar's
        ToolbarItem(placement: .principal) { Color.clear.frame(width: 1, height: 1).accessibilityHidden(true) }
        ToolbarItemGroup(placement: .topBarTrailing) {
            if model.currentAccess.canCreate {
                Button { router.newDoc() } label: { Image(systemName: "square.and.pencil") }
                    .accessibilityLabel("New doc")
                    .help("New doc (⌘N)")
            }
            if model.currentAccess.canEdit {
                Button { capturing = true } label: { Image(systemName: "plus.circle") }
                    .accessibilityLabel("Capture a to-do")
                    .accessibilityIdentifier("today.capture")
            }
        }
        // the sidebar has Settings under the split view
        if !router.isPad {
            ToolbarSpacer(.fixed, placement: .topBarTrailing)
            ToolbarItem(placement: .topBarTrailing) {
                Button { router.showSettings = true } label: {
                    Text(Principal.initial(model.home.profileName ?? "Taisce"))
                        .font(.callout.weight(.semibold))
                        .foregroundStyle(Color.lapis.accent)
                }
                .accessibilityLabel("Account and settings")
                .accessibilityIdentifier("today.settings")
            }
        }
    }

    private func reload() async {
        board = await model.loadTodos().board
    }
}

/// A pinned doc with where it lives and who touched it last.
struct PinnedCard: Identifiable, Hashable {
    var doc: DocCardModel
    var crumb: String?
    /// the workspace's stored colour
    var color: String?
    var author: String?
    var id: DocID { doc.id }

    /// "keeper:API · 12:41"
    func meta(now: Date = .now) -> String? {
        let parts = [author, doc.edited.map { RelativeTime.string($0, now: now) }].compactMap { $0 }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }
}

struct TodayContent: View {
    let dayLine: String
    let badge: SyncBadge
    /// nil while the queue loads (the card waits rather than flashing empty)
    var review: [ReviewQueueItem]?
    var reading: TodayBook?
    var readingCover: UIImage?
    /// nil while loading
    let due: [TodoEntry]?
    let pinned: [PinnedCard]
    var now: Date = .now
    var onReview: () -> Void = {}
    var onOpen: (DocID) -> Void = { _ in }
    var onRead: (TodayBook) -> Void = { _ in }
    var onBooks: () -> Void = {}
    var onAllTodos: () -> Void = {}
    var onUnpin: (DocID) -> Void = { _ in }
    /// rows checked by a ring tap, fading out
    var checked: Set<String> = []
    var onToggleDone: ((TodoEntry) -> Void)?

    static let reviewRows = 3

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 22) {
                header
                if let review, !review.isEmpty { reviewCard(review) }
                if let reading { readingSection(reading) }
                dueSection
                pinnedSection
            }
            .padding(.top, 4)
            .padding(.bottom, 32)
            .frame(maxWidth: Lapis.Layout.readingWidth)
            .frame(maxWidth: .infinity)
        }
        .groundBackground()
        .navigationTitle("Today")
        .navigationBarTitleDisplayMode(.inline)
        .animation(Lapis.Motion.standard, value: review?.count)
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(dayLine.uppercased())
                    .lapisFont(.label)
                    .foregroundStyle(Color.lapis.ink3)
                    .accessibilityLabel(dayLine)
                Spacer(minLength: 8)
                SyncDotLabel(badge: badge)
            }
            Text("Today")
                .lapisFont(.display)
                .foregroundStyle(Color.lapis.ink)
                .accessibilityAddTraits(.isHeader)
        }
        .padding(.horizontal, Lapis.Layout.gutter)
    }

    // MARK: review

    private func reviewCard(_ items: [ReviewQueueItem]) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            Button(action: onReview) {
                HStack(spacing: 12) {
                    Image(systemName: "sparkle")
                        .font(.body.weight(.semibold))
                        .foregroundStyle(Color.lapis.agent)
                        .frame(width: 34, height: 34)
                        .background(Color.lapis.agentSoft, in: .rect(cornerRadius: Lapis.Radius.field, style: .continuous))
                        .accessibilityHidden(true)
                    VStack(alignment: .leading, spacing: 1) {
                        Text(ReviewCopy.title(items)).font(.headline).foregroundStyle(Color.lapis.ink)
                        Text(ReviewCopy.subtitle(ReviewQueueSummary(items))).font(.subheadline).foregroundStyle(Color.lapis.ink3)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    Image(systemName: "chevron.right").font(.footnote.weight(.semibold)).foregroundStyle(Color.lapis.ink3).accessibilityHidden(true)
                }
                .frame(minHeight: Lapis.Layout.minTarget)
                .contentShape(.rect)
            }
            .buttonStyle(.plain)
            .accessibilityHint("Opens the review list")
            .accessibilityIdentifier("today.review")
            ForEach(items.prefix(Self.reviewRows)) { item in
                Rectangle().fill(Color.lapis.line2).frame(height: 1)
                Button { onOpen(item.docID) } label: { ReviewRowLabel(item: item) }
                    .buttonStyle(.plain)
            }
            if items.count > Self.reviewRows {
                Rectangle().fill(Color.lapis.line2).frame(height: 1)
                Button(action: onReview) {
                    Text("\(items.count - Self.reviewRows) more").font(.subheadline.weight(.medium)).foregroundStyle(Color.lapis.accent)
                        .frame(maxWidth: .infinity, minHeight: Lapis.Layout.minTarget, alignment: .leading)
                        .contentShape(.rect)
                }
                .buttonStyle(.plain)
            }
        }
        .padding(.horizontal, 16)
        .padding(.top, 12)
        .padding(.bottom, 4)
        .background(Color.lapis.card, in: .rect(cornerRadius: Lapis.Radius.tab, style: .continuous))
        .padding(.horizontal, 16)
        .transition(.opacity)
    }

    // MARK: reading

    private func readingSection(_ book: TodayBook) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            SectionHeading("Continue reading", link: "Books", action: onBooks)
            Button { onRead(book) } label: {
                HStack(spacing: 14) {
                    TodayCover(book: book, image: readingCover).frame(width: 54)
                    VStack(alignment: .leading, spacing: 3) {
                        Text(book.title).lapisFont(.cardTitle).foregroundStyle(Color.lapis.ink).lineLimit(2)
                        let byline = ContinueReading.byline(book)
                        if !byline.isEmpty {
                            Text(byline).font(.subheadline).foregroundStyle(Color.lapis.ink3).lineLimit(2)
                        }
                        ProgressBar(value: book.percentage / 100, label: "Reading progress").padding(.top, 8)
                        HStack(spacing: 0) {
                            Text(ContinueReading.percent(book)).fontWeight(.semibold).foregroundStyle(Color.lapis.ink)
                            if let w = ContinueReading.whereAndWhen(book, now: now) {
                                Text(" · \(w)").foregroundStyle(Color.lapis.ink3)
                            }
                        }
                        .font(.footnote)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    Text("Read")
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(Color.lapis.onAccent)
                        .padding(.horizontal, 14)
                        .frame(height: 34)
                        .background(Color.lapis.accent, in: .capsule)
                        .accessibilityHidden(true)
                }
                .padding(14)
                .background(Color.lapis.card, in: .rect(cornerRadius: Lapis.Radius.tab, style: .continuous))
                .contentShape(.rect(cornerRadius: Lapis.Radius.tab))
            }
            .buttonStyle(.plain)
            .accessibilityElement(children: .combine)
            .accessibilityLabel("Continue reading \(book.title), \(ContinueReading.percent(book))")
            .accessibilityHint("Opens the reader")
            .accessibilityIdentifier("today.continueReading")
            .padding(.horizontal, 16)
        }
    }

    // MARK: due

    @ViewBuilder private var dueSection: some View {
        VStack(alignment: .leading, spacing: 0) {
            SectionHeading("Due", link: "All to-dos", action: onAllTodos)
            Group {
                if let due {
                    if due.isEmpty {
                        EmptyCard(icon: "checkmark.circle", title: "Nothing due", hint: "Deadlines show up here. Add one to any to-do, like \u{2018}due fri 3pm\u{2019}.")
                    } else {
                        GroupedRows(items: Array(due.prefix(TodayDue.limit))) { entry in
                            // the ring is its own button; the rest of the row opens To-dos
                            TodoRow(entry: entry, now: now, done: checked.contains(entry.id), onToggle: onToggleDone.map { f in { f(entry) } })
                                .onTapGesture(perform: onAllTodos)
                                .accessibilityAction(named: "Open To-dos", onAllTodos)
                        }
                        if due.count > TodayDue.limit {
                            Button(action: onAllTodos) {
                                Text("\(due.count - TodayDue.limit) more in To-dos").font(.footnote).foregroundStyle(Color.lapis.ink3)
                                    .frame(minHeight: Lapis.Layout.minTarget)
                            }
                            .buttonStyle(.plain)
                            .padding(.horizontal, 4)
                        }
                    }
                } else {
                    SkeletonBlock(height: 104)
                }
            }
            .padding(.horizontal, 16)
        }
    }

    // MARK: pinned

    @ViewBuilder private var pinnedSection: some View {
        VStack(alignment: .leading, spacing: 0) {
            SectionHeading("Pinned")
            Group {
                if pinned.isEmpty {
                    EmptyCard(icon: "pin", title: "No pinned docs", hint: "Long-press a doc in Library to pin it here. Pins stay on this device.")
                } else {
                    LazyVGrid(columns: [GridItem(.adaptive(minimum: 150), spacing: 12)], spacing: 12) {
                        ForEach(pinned) { card in
                            Button { onOpen(card.id) } label: { PinnedCardView(card: card, now: now) }
                                .buttonStyle(.plain)
                                .contextMenu {
                                    Button("Unpin from Today", systemImage: "pin.slash") { onUnpin(card.id) }
                                }
                        }
                    }
                }
            }
            .padding(.horizontal, 16)
        }
    }
}

/// "Due ··· All to-dos ›": a bold section title and an optional link.
struct SectionHeading: View {
    let title: String
    var link: String?
    var action: () -> Void = {}

    init(_ title: String, link: String? = nil, action: @escaping () -> Void = {}) {
        self.title = title
        self.link = link
        self.action = action
    }

    var body: some View {
        HStack {
            Text(title)
                .font(.title3.weight(.bold))
                .foregroundStyle(Color.lapis.ink)
                .accessibilityAddTraits(.isHeader)
            Spacer()
            if let link {
                Button(action: action) {
                    HStack(spacing: 2) {
                        Text(link)
                        Image(systemName: "chevron.right").font(.footnote.weight(.semibold)).accessibilityHidden(true)
                    }
                    .font(.subheadline)
                    .foregroundStyle(Color.lapis.accent)
                    .frame(minHeight: Lapis.Layout.minTarget)
                    .contentShape(.rect)
                }
                .buttonStyle(.plain)
            }
        }
        .frame(minHeight: Lapis.Layout.minTarget)
        .padding(.horizontal, Lapis.Layout.gutter)
        .padding(.bottom, 4)
    }
}

/// The sync state as a dot and a word ("● Synced").
struct SyncDotLabel: View {
    let badge: SyncBadge

    var color: Color {
        switch badge.tone {
        case .saved: Color.lapis.green
        case .busy: Color.lapis.accent
        case .offline: Color.lapis.yellowMark
        case .idle: Color.lapis.ink3
        }
    }

    /// Today says "Synced" for the editor's "Saved".
    var text: String { badge.tone == .saved ? "Synced" : badge.text }

    var body: some View {
        HStack(spacing: 5) {
            Circle().fill(color).frame(width: 7, height: 7).accessibilityHidden(true)
            Text(text).font(.footnote).foregroundStyle(Color.lapis.ink3)
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Sync: \(text)")
    }
}

/// A queued change on Today's card: verdict, doc, who.
struct ReviewRowLabel: View {
    let item: ReviewQueueItem
    @Environment(\.dynamicTypeSize) private var typeSize

    var body: some View {
        // accessibility text sizes: chip, title and who stacked, each whole
        let layout = typeSize.isAccessibilitySize
            ? AnyLayout(VStackLayout(alignment: .leading, spacing: 4))
            : AnyLayout(HStackLayout(spacing: 10))
        layout {
            VerdictChip(ReviewCopy.verdict(item.kind), short: true)
            Text(item.docTitle.isEmpty ? "Untitled" : item.docTitle)
                .lapisFont(.cardTitle, weight: .medium)
                .foregroundStyle(Color.lapis.ink)
                .lineLimit(typeSize.isAccessibilitySize ? 3 : 1)
                .frame(maxWidth: .infinity, alignment: .leading)
            Text(item.proposer)
                .font(.lapisMono(.caption))
                .foregroundStyle(item.byAgent ? Color.lapis.agent : Color.lapis.ink3)
                .lineLimit(1)
        }
        .padding(.vertical, typeSize.isAccessibilitySize ? 8 : 0)
        .frame(minHeight: Lapis.Layout.minTarget)
        .contentShape(.rect)
        .accessibilityElement(children: .combine)
        .accessibilityHint("Opens the doc")
    }
}

/// A pinned doc: workspace and folder, the serif title, who and when.
struct PinnedCardView: View {
    let card: PinnedCard
    var now: Date = .now

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            if let crumb = card.crumb {
                HStack(spacing: 5) {
                    if let color = Color(workspaceColor: card.color) {
                        RoundedRectangle(cornerRadius: 3, style: .continuous).fill(color).frame(width: 8, height: 8).accessibilityHidden(true)
                    }
                    Text(crumb).font(.caption).foregroundStyle(Color.lapis.ink3).lineLimit(1)
                }
            }
            Text(card.doc.title)
                .lapisFont(.cardTitle)
                .foregroundStyle(Color.lapis.ink)
                .multilineTextAlignment(.leading)
                .lineLimit(3)
            Spacer(minLength: 8)
            if let meta = card.meta(now: now) {
                Text(meta).font(.footnote).foregroundStyle(Color.lapis.ink3).lineLimit(1)
            }
        }
        .padding(.horizontal, 14)
        .padding(.top, 14)
        .padding(.bottom, 12)
        .frame(maxWidth: .infinity, minHeight: 112, alignment: .topLeading)
        .background(Color.lapis.card, in: .rect(cornerRadius: Lapis.Radius.xl, style: .continuous))
        .hoverWash(radius: Lapis.Radius.xl)
        .contentShape(.rect(cornerRadius: Lapis.Radius.xl))
        .accessibilityElement(children: .combine)
    }
}

/// The Continue reading cover: the server's colour cover when it loaded,
/// else the typographic one (decision 3).
struct TodayCover: View {
    let book: TodayBook
    var image: UIImage?

    var body: some View {
        if let image {
            Color.clear
                .aspectRatio(2.0 / 3.0, contentMode: .fit)
                .overlay { Image(uiImage: image).resizable().scaledToFill() }
                .clipShape(.rect(cornerRadius: 4, style: .continuous))
                .shadow(color: Color.lapis.shadow, radius: 6, y: 4)
                .accessibilityLabel("Cover of \(book.title)")
        } else {
            BookCover(id: book.id, title: book.title, author: book.author.isEmpty ? nil : book.author)
        }
    }
}

/// "Capture a to-do": one line onto today's list, with the server's
/// reading of any `due …` phrase.
struct CaptureTodoSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var draft = ""
    @State private var hint: TodoParseHint?
    @FocusState private var focused: Bool

    var body: some View {
        NavigationStack {
            VStack(alignment: .leading, spacing: 12) {
                TodoComposer(draft: $draft, hint: hint, focused: $focused, onAdd: add)
                Text("Goes on today's list. \u{2018}due fri 3pm\u{2019} sets a deadline.")
                    .font(.footnote)
                    .foregroundStyle(Color.lapis.ink3)
                    .padding(.horizontal, 4)
                Spacer()
            }
            .padding(16)
            .groundBackground()
            .navigationTitle("Capture a to-do")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Add", action: add).disabled(draft.trimmingCharacters(in: .whitespaces).isEmpty)
                }
            }
            .onAppear { focused = true }
            .task(id: draft) {
                guard draft.count > 2 else { hint = nil; return }
                try? await Task.sleep(for: .milliseconds(300))
                guard !Task.isCancelled else { return }
                hint = await model.parseHint(draft)
            }
        }
        .presentationDetents([.height(240), .medium])
    }

    private func add() {
        let text = draft
        guard !text.trimmingCharacters(in: .whitespaces).isEmpty else { return }
        Haptics.success()
        Task { await model.addTodo(text) }
        dismiss()
    }
}

#if DEBUG
extension PreviewData {
    static let reviewItems: [ReviewQueueItem] = [
        ReviewQueueItem(id: "r1", docID: "g-arch", docTitle: "Architecture", kind: .flagged, proposer: "keeper:API", summary: "Edit a paragraph"),
        ReviewQueueItem(id: "r2", docID: "g-road", docTitle: "Roadmap", kind: .flagged, proposer: "claude:docs", summary: "Add a paragraph"),
        ReviewQueueItem(id: "r3", docID: "home-garden", docTitle: "Garden plan", kind: .parked, proposer: "claude:home", summary: "Move \u{201C}Garden plan\u{201D} to the Trash"),
    ]

    static let book = TodayBook(id: "b1", title: "The First Heretic", author: "Aaron Dembski-Bowden", series: "The Horus Heresy", seriesIndex: 14, percentage: 64, device: "Kindle", updatedAt: "2026-09-21T10:00:00Z")

    static var pinnedCards: [PinnedCard] {
        cards.map { PinnedCard(doc: $0, crumb: index.breadcrumb(of: $0.id), color: "accent", author: "keeper:API") }
    }
}

#Preview("Today") {
    NavigationStack {
        TodayContent(
            dayLine: RelativeTime.dayLine(PreviewData.now), badge: SyncBadge(tone: .saved, text: "Saved"),
            review: PreviewData.reviewItems, reading: PreviewData.book,
            due: TodayDue.rows(PreviewData.board, now: PreviewData.now), pinned: PreviewData.pinnedCards, now: PreviewData.now
        )
    }
    .preferredColorScheme(.light)
}

#Preview("Today, dark") {
    NavigationStack {
        TodayContent(
            dayLine: RelativeTime.dayLine(PreviewData.now), badge: SyncBadge(tone: .saved, text: "Saved"),
            review: PreviewData.reviewItems, reading: PreviewData.book,
            due: TodayDue.rows(PreviewData.board, now: PreviewData.now), pinned: PreviewData.pinnedCards, now: PreviewData.now
        )
    }
    .preferredColorScheme(.dark)
}

#Preview("Today, empty") {
    NavigationStack {
        TodayContent(
            dayLine: RelativeTime.dayLine(PreviewData.now), badge: SyncBadge(tone: .offline, text: "Offline · 2 pending"),
            review: [], due: [], pinned: [], now: PreviewData.now
        )
    }
    .preferredColorScheme(.light)
}
#endif
