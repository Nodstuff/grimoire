import SwiftUI
import TaisceKit

/// Overdue / Today / Upcoming. Swipe right marks done, swipe left snoozes
/// (or More); all queue through the outbox. The field at the top adds to
/// today, with the server's reading of any `due …` phrase as a live pill.
struct TodosScreen: View {
    @Environment(AppModel.self) private var model
    @Environment(Router.self) private var router
    @State private var board: TodoBoard?
    @State private var offline = false
    @State private var draft = ""
    @State private var hint: TodoParseHint?
    @State private var taps = RingTaps()
    @State private var filter: TodoFilter = .all

    var body: some View {
        TodosContent(
            board: TodoBoard.shown(board, hasSynced: model.hasSynced, status: model.syncStatus),
            offline: offline,
            readOnly: !model.currentAccess.canEdit,
            alertStatus: model.dueAlerts.status,
            onEnableAlerts: { Task { await model.dueAlerts.requestAuthorization() } },
            draft: $draft,
            hint: hint,
            filter: $filter,
            onDone: { e in Task { await model.markDone(e) } },
            checked: taps.checked,
            onToggleDone: { e in taps.tap(e) { await model.markDone($0) } },
            onSnooze: { e, s in Task { await model.snooze(e, s) } },
            onShowList: model.todoDocID.map { id in { router.open(.doc(id)) } },
            onAdd: {
                let text = draft
                draft = ""
                hint = nil
                Task { await model.addTodo(text) }
            }
        )
        .safeAreaInset(edge: .top, spacing: 0) { WorkspaceBar() } // workspaces
        .refreshable { await reload() }
        .task(id: model.todoRevision) { await reload() }
        .task { await model.dueAlerts.refresh() }
        .task(id: draft) {
            // debounce typing, then ask the server how it reads the phrase
            guard draft.count > 2 else { hint = nil; return }
            try? await Task.sleep(for: .milliseconds(300))
            guard !Task.isCancelled else { return }
            hint = await model.parseHint(draft)
        }
    }

    private func reload() async {
        let r = await model.loadTodos()
        board = r.board
        offline = r.offline
    }
}

/// Which groups the list shows (the filter menu).
enum TodoFilter: String, CaseIterable, Hashable {
    case all, overdue, today, upcoming

    var title: String {
        switch self {
        case .all: "All"
        case .overdue: "Overdue"
        case .today: "Today"
        case .upcoming: "Upcoming"
        }
    }

    func shows(_ group: TodoFilter) -> Bool { self == .all || self == group }
}

struct TodosContent: View {
    /// nil while loading
    let board: TodoBoard?
    var offline = false
    /// a workspace you only view (ADR 0004): no add, done, snooze
    var readOnly = false
    var alertStatus: DueAlertStatus = .allowed
    var onEnableAlerts: () -> Void = {}
    @Binding var draft: String
    var hint: TodoParseHint?
    var filter: Binding<TodoFilter> = .constant(.all)
    var now: Date = .now
    var onDone: (TodoEntry) -> Void = { _ in }
    /// rows checked by a ring tap, fading out
    var checked: Set<String> = []
    var onToggleDone: ((TodoEntry) -> Void)?
    var onSnooze: (TodoEntry, Snooze) -> Void = { _, _ in }
    /// open the To-do doc itself (nil before it exists)
    var onShowList: (() -> Void)?
    var onAdd: () -> Void = {}

    @State private var snoozing: TodoEntry?
    @State private var more: TodoEntry?
    @FocusState private var fieldFocused: Bool

    static func kicker(offline: Bool, readOnly: Bool) -> String? {
        switch (offline, readOnly) {
        case (true, true): "View only · offline"
        case (false, true): "View only"
        case (true, false): "Offline · from this device"
        case (false, false): nil
        }
    }

    var body: some View {
        List {
            Section {
                header
                    .listRowBackground(Color.clear)
                    .listRowInsets(EdgeInsets(top: 0, leading: 4, bottom: 0, trailing: 4))
                    .listRowSeparator(.hidden)
                if !readOnly {
                    TodoComposer(draft: $draft, hint: hint, now: now, focused: $fieldFocused, onAdd: onAdd)
                        .listRowBackground(Color.clear)
                        .listRowInsets(EdgeInsets(top: 14, leading: 0, bottom: 0, trailing: 0))
                        .listRowSeparator(.hidden)
                }
            }
            if DueAlertPrompt(alertStatus).showsTodosCard {
                Section {
                    PromptCard(icon: "bell", text: "Get a nudge when things are due", action: "Turn on", onAction: onEnableAlerts)
                        .listRowBackground(Color.clear)
                        .listRowInsets(EdgeInsets())
                }
            }
            if let board {
                if board.isEmpty {
                    Section {
                        EmptyCard(icon: "checkmark.circle", title: "All clear", hint: readOnly ? "Nothing open in this list." : "Nothing open. Add a to-do above; \u{2018}due fri 3pm\u{2019} sets a deadline.")
                            .listRowBackground(Color.clear)
                            .listRowInsets(EdgeInsets())
                    }
                }
                if filter.wrappedValue.shows(.overdue) { section("Overdue", board.overdue, color: Color.lapis.red) }
                if filter.wrappedValue.shows(.today) { section("Today", board.today, color: Color.lapis.ink) }
                if filter.wrappedValue.shows(.upcoming) { section("Upcoming", board.upcoming, color: Color.lapis.ink) }
            } else {
                Section {
                    SkeletonBlock(height: 156, radius: Lapis.Radius.tab)
                        .listRowBackground(Color.clear)
                        .listRowInsets(EdgeInsets())
                }
            }
        }
        .listStyle(.insetGrouped)
        .listSectionSpacing(18)
        .scrollContentBackground(.hidden)
        .contentMargins(.top, 0, for: .scrollContent)
        .environment(\.defaultMinListRowHeight, 52)
        // reading width on iPad, centred on the ground
        .frame(maxWidth: Lapis.Layout.readingWidth)
        .frame(maxWidth: .infinity)
        .groundBackground()
        .navigationTitle("To-dos")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar { toolbar }
        .confirmationDialog("Snooze until", isPresented: Binding(get: { snoozing != nil }, set: { if !$0 { snoozing = nil } }), titleVisibility: .visible, presenting: snoozing) { entry in
            ForEach(Snooze.allCases, id: \.self) { s in
                Button(s.title) { onSnooze(entry, s) }
            }
        }
        .confirmationDialog(more?.text ?? "", isPresented: Binding(get: { more != nil }, set: { if !$0 { more = nil } }), titleVisibility: .visible, presenting: more) { entry in
            Button("Done") { onDone(entry) }
            ForEach(Snooze.allCases, id: \.self) { s in
                Button("Snooze \(s.title.lowercased())") { onSnooze(entry, s) }
            }
            Button("Copy text") { UIPasteboard.general.string = entry.text }
            if let onShowList { Button("Show in the To-do doc", action: onShowList) }
        }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text("To-dos")
                .lapisFont(.display)
                .foregroundStyle(Color.lapis.ink)
                .accessibilityAddTraits(.isHeader)
            let line = [TodosContent.kicker(offline: offline, readOnly: readOnly), board.map(TodosSummary.line)].compactMap { $0 }
            if !line.isEmpty {
                Text(line.joined(separator: " · ")).font(.subheadline).foregroundStyle(Color.lapis.ink3)
            }
        }
        .padding(.horizontal, 16)
    }

    @ToolbarContentBuilder private var toolbar: some ToolbarContent {
        ToolbarItem(placement: .principal) { Color.clear.frame(width: 1, height: 1).accessibilityHidden(true) }
        ToolbarItemGroup(placement: .topBarTrailing) {
            Menu {
                Picker("Show", selection: filter) {
                    ForEach(TodoFilter.allCases, id: \.self) { Text($0.title).tag($0) }
                }
            } label: {
                Image(systemName: filter.wrappedValue == .all ? "line.3.horizontal.decrease" : "line.3.horizontal.decrease.circle.fill")
            }
            .accessibilityLabel("Filter")
            .accessibilityValue(filter.wrappedValue.title)
            Menu {
                if let onShowList { Button("Show the To-do doc", systemImage: "doc.text", action: onShowList) }
                if DueAlertPrompt(alertStatus).showsTodosCard {
                    Button("Turn on due alerts", systemImage: "bell", action: onEnableAlerts)
                }
            } label: {
                Image(systemName: "ellipsis")
            }
            .accessibilityLabel("More")
        }
    }

    @ViewBuilder func section(_ title: String, _ items: [TodoEntry], color: Color) -> some View {
        if !items.isEmpty {
            Section {
                ForEach(items) { entry in
                    if readOnly {
                        TodoRow(entry: entry, now: now, done: false, shortPill: true, onToggle: nil)
                            .listRowInsets(EdgeInsets())
                            .listRowBackground(Color.lapis.card)
                            .listRowSeparatorTint(Color.lapis.line2)
                    } else {
                        TodoRow(entry: entry, now: now, done: checked.contains(entry.id), shortPill: true, onToggle: onToggleDone.map { f in { f(entry) } })
                            .listRowInsets(EdgeInsets())
                            .listRowBackground(Color.lapis.card)
                            .listRowSeparatorTint(Color.lapis.line2)
                            .swipeActions(edge: .leading, allowsFullSwipe: true) {
                                Button("Done", systemImage: "checkmark") { onDone(entry) }.tint(Color.lapis.greenFill)
                            }
                            .swipeActions(edge: .trailing, allowsFullSwipe: false) {
                                Button("More", systemImage: "ellipsis") { more = entry }.tint(Color.lapis.ink3)
                                Button("Snooze", systemImage: "timer") { snoozing = entry }.tint(Color.lapis.yellowFill)
                            }
                            .contextMenu {
                                Button("Done", systemImage: "checkmark") { onDone(entry) }
                                Menu("Snooze", systemImage: "timer") {
                                    ForEach(Snooze.allCases, id: \.self) { s in Button(s.title) { onSnooze(entry, s) } }
                                }
                                Button("Copy text", systemImage: "doc.on.doc") { UIPasteboard.general.string = entry.text }
                            }
                            .accessibilityAction(named: "Done") { onDone(entry) }
                            .accessibilityAction(named: "Snooze") { snoozing = entry }
                    }
                }
            } header: {
                HStack(spacing: 6) {
                    Text(title).foregroundStyle(color)
                    Text("\(items.count)").foregroundStyle(Color.lapis.ink3).monospacedDigit()
                }
                .font(.headline)
                .textCase(nil)
                .accessibilityElement(children: .combine)
                .accessibilityAddTraits(.isHeader)
            }
        }
    }
}

/// "Add a to-do": the field in a card, the parsed deadline as a pill under
/// it, and where it goes. Return adds.
struct TodoComposer: View {
    @Binding var draft: String
    var hint: TodoParseHint?
    var now: Date = .now
    var focused: FocusState<Bool>.Binding
    var onAdd: () -> Void

    var dueText: String? {
        guard let hint, hint.warning == nil, let due = hint.due() else { return nil }
        return TodoComposer.format(due)
    }

    /// "Fri 30 Oct, 10:00" / "Fri 30 Oct"
    static func format(_ due: Due, calendar: Calendar = .current) -> String {
        let day = DueLabel.shortDate(due, calendar: calendar)
        guard let h = due.hour, let m = due.minute else { return day }
        return "\(day), " + String(format: "%02d:%02d", h, m)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 12) {
                Button(action: { draft.isEmpty ? (focused.wrappedValue = true) : onAdd() }) {
                    Image(systemName: draft.isEmpty ? "plus" : "arrow.up")
                        .font(.footnote.weight(.bold))
                        .foregroundStyle(Color.lapis.onAccent)
                        .frame(width: 24, height: 24)
                        .background(Color.lapis.accent, in: .circle)
                        .frame(width: Lapis.Layout.minTarget, height: Lapis.Layout.minTarget)
                        .contentShape(.rect)
                }
                .buttonStyle(.plain)
                .padding(.horizontal, -10)
                .accessibilityLabel(draft.isEmpty ? "New to-do" : "Add to-do")
                TextField("Add a to-do\u{2026} \u{2018}due fri 3pm\u{2019}", text: $draft, axis: .vertical)
                    .focused(focused)
                    .submitLabel(.done)
                    .onSubmit(onAdd)
                    .font(.body)
                    .foregroundStyle(Color.lapis.ink)
                    .lineLimit(1...4)
                    .accessibilityLabel("Add a to-do")
                    .accessibilityIdentifier("todos.add")
            }
            if !draft.isEmpty {
                HStack(spacing: 8) {
                    if let dueText {
                        Label(dueText, systemImage: "calendar")
                            .font(.footnote.weight(.semibold))
                            .foregroundStyle(Color.lapis.accent)
                            .padding(.horizontal, 8)
                            .padding(.vertical, 3)
                            .background(Color.lapis.accentSoft, in: .capsule)
                            .accessibilityLabel("Due \(dueText)")
                    } else if let warning = hint?.warning {
                        Label(warning, systemImage: "exclamationmark.triangle")
                            .font(.footnote)
                            .foregroundStyle(Color.lapis.yellow)
                    }
                    Text("into today's list").font(.footnote).foregroundStyle(Color.lapis.ink3)
                }
                .padding(.leading, 36)
                .transition(.opacity)
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 8)
        .background(Color.lapis.card, in: .rect(cornerRadius: Lapis.Radius.tab, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: Lapis.Radius.tab, style: .continuous)
                .strokeBorder(focused.wrappedValue ? Color.lapis.accentRing : Color.lapis.line, lineWidth: focused.wrappedValue ? 2 : 1)
        }
        .animation(Lapis.Motion.standard, value: dueText)
        .animation(Lapis.Motion.standard, value: draft.isEmpty)
    }
}

#if DEBUG
#Preview("To-dos") {
    @Previewable @State var draft = ""
    NavigationStack {
        TodosContent(board: PreviewData.board, alertStatus: .notDetermined, draft: $draft, now: PreviewData.now)
    }
    .preferredColorScheme(.light)
}

#Preview("To-dos, typing, dark") {
    @Previewable @State var draft = "Call Ann due fri 3pm"
    NavigationStack {
        TodosContent(
            board: PreviewData.board, draft: $draft,
            hint: TodoParseHint(text: "Call Ann", deadline: PreviewData.day(2).dateString, dueTime: "15:00"), now: PreviewData.now
        )
    }
    .preferredColorScheme(.dark)
}

#Preview("To-dos, empty") {
    @Previewable @State var draft = ""
    NavigationStack { TodosContent(board: TodoBoard(), offline: true, draft: $draft) }
        .preferredColorScheme(.dark)
}
#endif
