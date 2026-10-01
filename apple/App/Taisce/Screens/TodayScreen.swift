import SwiftUI
import TaisceKit

/// Today: what's due and the pinned docs. Due items come from the server's
/// read-only due list (the cache offline); pins are local.
struct TodayScreen: View {
    @Environment(AppModel.self) private var model
    @Environment(Router.self) private var router
    @State private var board: TodoBoard?

    var body: some View {
        TodayContent(
            dayLine: RelativeTime.dayLine(),
            due: TodoBoard.shown(board, hasSynced: model.hasSynced, status: model.syncStatus)?.due,
            pinned: DocCardModel.pinned(model.pins, index: model.index, meta: model.editMeta),
            badge: SyncBadge.make(status: model.syncStatus, pending: model.pendingWrites),
            onSearch: router.showSearch,
            onSettings: { router.showSettings = true },
            onAllTodos: router.showTodos,
            onOpen: { router.open(.doc($0)) },
            onUnpin: model.togglePin
        )
        .refreshable { await reload() }
        .task(id: model.todoRevision) { await reload() }
        .task(id: model.pins) {
            for id in model.pins { await model.loadEditMeta(id) }
        }
    }

    private func reload() async {
        board = await model.loadTodos().board
    }
}

struct TodayContent: View {
    let dayLine: String
    /// nil while loading
    let due: [TodoEntry]?
    let pinned: [DocCardModel]
    let badge: SyncBadge
    var now: Date = .now
    var onSearch: () -> Void = {}
    var onSettings: () -> Void = {}
    var onAllTodos: () -> Void = {}
    var onOpen: (DocID) -> Void = { _ in }
    var onUnpin: (DocID) -> Void = { _ in }

    static let dueLimit = 5


    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 24) {
                VStack(alignment: .leading, spacing: 4) {
                    HStack(spacing: 6) {
                        Text(dayLine).font(.caption).foregroundStyle(Theme.secondary)
                        Spacer()
                        SyncChip(badge: badge)
                        CircleIconButton(systemImage: "gearshape", label: "Settings", action: onSettings)
                    }
                    Text("Today")
                        .font(Theme.serif(.largeTitle))
                        .foregroundStyle(Theme.text)
                        .accessibilityAddTraits(.isHeader)
                        .padding(.bottom, 10)
                    SearchFieldButton(action: onSearch)
                }
                dueSection
                pinnedSection
            }
            .padding(.horizontal, Theme.gutter)
            .padding(.top, 4)
            .padding(.bottom, 32)
            .frame(maxWidth: Theme.readingWidth)
            .frame(maxWidth: .infinity)
        }
        .groundBackground()
        .toolbarVisibility(.hidden, for: .navigationBar)
    }

    @ViewBuilder private var dueSection: some View {
        VStack(alignment: .leading, spacing: 10) {
            SectionLabel("Due", trailing: AnyView(
                Button(action: onAllTodos) {
                    Text("All to-dos")
                    .font(.caption.weight(.medium))
                    .foregroundStyle(Theme.accentActive)
                    .frame(minHeight: Theme.minTarget)
                    .contentShape(.rect)
                }
                .buttonStyle(.plain)
            ))
            if let due {
                if due.isEmpty {
                    EmptyCard(icon: "checkmark.circle", title: "Nothing due", hint: "Deadlines show up here. Add one to any to-do, like \u{2018}due fri 3pm\u{2019}.")
                } else {
                    CardList(items: Array(due.prefix(Self.dueLimit))) { entry in
                        Button(action: onAllTodos) { TodoRow(entry: entry, now: now) }.buttonStyle(.plain)
                    }
                    if due.count > Self.dueLimit {
                        Text("\(due.count - Self.dueLimit) more in To-dos").font(.footnote).foregroundStyle(Theme.secondary).padding(.horizontal, 4)
                    }
                }
            } else {
                ProgressView().frame(maxWidth: .infinity, minHeight: 80)
            }
        }
    }

    @ViewBuilder private var pinnedSection: some View {
        VStack(alignment: .leading, spacing: 10) {
            SectionLabel("Pinned")
            if pinned.isEmpty {
                EmptyCard(icon: "pin", title: "No pinned docs", hint: "Long-press a doc in Library to pin it here. Pins stay on this device.")
            } else {
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 150), spacing: 10)], spacing: 10) {
                    ForEach(pinned) { doc in
                        Button { onOpen(doc.id) } label: { DocCard(doc: doc, now: now) }
                            .buttonStyle(.plain)
                            .contextMenu {
                                Button("Unpin", systemImage: "pin.slash") { onUnpin(doc.id) }
                            }
                    }
                }
            }
        }
    }
}

#if DEBUG
#Preview("Today") {
    NavigationStack {
        TodayContent(
            dayLine: RelativeTime.dayLine(PreviewData.now), due: PreviewData.board.due, pinned: PreviewData.cards,
            badge: SyncBadge(tone: .saved, text: "Saved"), now: PreviewData.now
        )
    }
    .preferredColorScheme(.dark)
}

#Preview("Today, empty, light") {
    NavigationStack {
        TodayContent(
            dayLine: RelativeTime.dayLine(PreviewData.now), due: [], pinned: [],
            badge: SyncBadge(tone: .offline, text: "Offline · 2 pending"), now: PreviewData.now
        )
    }
    .preferredColorScheme(.light)
}
#endif
