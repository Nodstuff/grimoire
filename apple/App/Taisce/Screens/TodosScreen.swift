import SwiftUI
import TaisceKit

/// Overdue / Today / Upcoming. Swipe right marks done, swipe left snoozes;
/// both queue through the outbox. The bottom field adds to today, with the
/// server's reading of any `due …` phrase as a live hint.
struct TodosScreen: View {
    @Environment(AppModel.self) private var model
    @State private var board: TodoBoard?
    @State private var offline = false
    @State private var draft = ""
    @State private var hint: TodoParseHint?

    var body: some View {
        TodosContent(
            board: TodoBoard.shown(board, hasSynced: model.hasSynced, status: model.syncStatus),
            offline: offline,
            alertStatus: model.dueAlerts.status,
            onEnableAlerts: { Task { await model.dueAlerts.requestAuthorization() } },
            draft: $draft,
            hint: hint,
            onDone: { e in Task { await model.markDone(e) } },
            onSnooze: { e, s in Task { await model.snooze(e, s) } },
            onAdd: {
                let text = draft
                draft = ""
                hint = nil
                Task { await model.addTodo(text) }
            }
        )
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

struct TodosContent: View {
    /// nil while loading
    let board: TodoBoard?
    var offline = false
    var alertStatus: DueAlertStatus = .allowed
    var onEnableAlerts: () -> Void = {}
    @Binding var draft: String
    var hint: TodoParseHint?
    var now: Date = .now
    var onDone: (TodoEntry) -> Void = { _ in }
    var onSnooze: (TodoEntry, Snooze) -> Void = { _, _ in }
    var onAdd: () -> Void = {}

    @State private var snoozing: TodoEntry?
    @FocusState private var fieldFocused: Bool

    var body: some View {
        List {
            Section {
                ScreenHeader(title: "To-dos", kicker: offline ? "Offline · from this device" : nil) {
                    CircleIconButton(systemImage: "plus", label: "New to-do", filled: true) { fieldFocused = true }
                }
                .listRowBackground(Color.clear)
                .listRowInsets(EdgeInsets(top: 0, leading: 4, bottom: 0, trailing: 0))
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
                        EmptyCard(icon: "checkmark.circle", title: "All clear", hint: "Nothing open. Add a to-do below; \u{2018}due fri 3pm\u{2019} sets a deadline.")
                            .listRowBackground(Color.clear)
                            .listRowInsets(EdgeInsets())
                    }
                }
                section("Overdue", board.overdue, color: Theme.rose)
                section("Today", board.today, color: Theme.secondary)
                section("Upcoming", board.upcoming, color: Theme.secondary)
            } else {
                Section { HStack { Spacer(); ProgressView(); Spacer() }.frame(minHeight: 120).listRowBackground(Color.clear) }
            }
        }
        .listStyle(.insetGrouped)
        .listSectionSpacing(20)
        .contentMargins(.top, 0, for: .scrollContent)
        // reading width on iPad, centred on the ground
        .frame(maxWidth: Theme.readingWidth)
        .frame(maxWidth: .infinity)
        .background(Theme.ground.ignoresSafeArea())
        .groundBackground()
        .toolbarVisibility(.hidden, for: .navigationBar)
        .safeAreaInset(edge: .bottom) {
            NewTodoField(draft: $draft, hint: hint, now: now, focused: $fieldFocused, onAdd: onAdd)
                .padding(.horizontal, Theme.gutter)
                .padding(.bottom, 8)
                .frame(maxWidth: Theme.readingWidth)
        }
        .confirmationDialog("Snooze until", isPresented: Binding(get: { snoozing != nil }, set: { if !$0 { snoozing = nil } }), titleVisibility: .visible, presenting: snoozing) { entry in
            ForEach(Snooze.allCases, id: \.self) { s in
                Button(s.title) { onSnooze(entry, s) }
            }
        }
    }

    @ViewBuilder func section(_ title: String, _ items: [TodoEntry], color: Color) -> some View {
        if !items.isEmpty {
            Section {
                ForEach(items) { entry in
                    TodoRow(entry: entry, now: now)
                        .listRowInsets(EdgeInsets())
                        .listRowBackground(Theme.surface)
                        .swipeActions(edge: .leading, allowsFullSwipe: true) {
                            Button("Done", systemImage: "checkmark") { onDone(entry) }.tint(Theme.green)
                        }
                        .swipeActions(edge: .trailing, allowsFullSwipe: false) {
                            Button("Snooze", systemImage: "clock") { snoozing = entry }.tint(Theme.amber)
                        }
                        .accessibilityAction(named: "Done") { onDone(entry) }
                        .accessibilityAction(named: "Snooze") { snoozing = entry }
                }
            } header: {
                Text(title.uppercased())
                    .font(.caption.weight(.semibold))
                    .tracking(1.4)
                    .foregroundStyle(color)
                    .accessibilityAddTraits(.isHeader)
            }
        }
    }
}

/// "New to-do… try 'due fri 3pm'", on glass above the tab bar.
struct NewTodoField: View {
    @Binding var draft: String
    var hint: TodoParseHint?
    var now: Date = .now
    var focused: FocusState<Bool>.Binding
    var onAdd: () -> Void

    var hintText: String? {
        guard let hint else { return nil }
        if let w = hint.warning { return w }
        guard let due = hint.due() else { return nil }
        let label = DueLabel.make(TodoEntry(date: "", itemID: "", text: hint.text, due: due), now: now)
        return label.text.map { "Due \($0.replacingOccurrences(of: " · ", with: " "))" }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            if let hintText, !draft.isEmpty {
                Label(hintText, systemImage: hint?.warning == nil ? "calendar" : "exclamationmark.triangle")
                    .font(.footnote)
                    .foregroundStyle(hint?.warning == nil ? Theme.accentActive : Theme.amber)
                    .padding(.horizontal, 6)
                    .transition(.opacity)
            }
            HStack(spacing: 10) {
                Image(systemName: "plus.circle").foregroundStyle(Theme.accent).accessibilityHidden(true)
                TextField("New to-do\u{2026} try \u{2018}due fri 3pm\u{2019}", text: $draft)
                    .focused(focused)
                    .submitLabel(.done)
                    .onSubmit(onAdd)
                    .foregroundStyle(Theme.text)
                if !draft.isEmpty {
                    Button(action: onAdd) {
                        Image(systemName: "arrow.up.circle.fill").font(.title2).foregroundStyle(Theme.accent)
                            .frame(width: Theme.minTarget, height: Theme.minTarget)
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Add to-do")
                }
            }
            .padding(.leading, 16)
            .padding(.trailing, draft.isEmpty ? 16 : 0)
            .font(.subheadline)
            .frame(minHeight: 46)
            .glassEffect(.regular, in: .rect(cornerRadius: 25, style: .continuous))
        }
        .animation(.default, value: hintText)
    }
}

#Preview("To-dos") {
    @Previewable @State var draft = ""
    NavigationStack {
        TodosContent(board: PreviewData.board, alertStatus: .notDetermined, draft: $draft, now: PreviewData.now)
    }
    .preferredColorScheme(.dark)
}

#Preview("To-dos, typing, light") {
    @Previewable @State var draft = "Call Ann due fri 3pm"
    NavigationStack {
        TodosContent(
            board: TodoBoard.build(dated: Array(PreviewData.entries.prefix(1)), now: PreviewData.now), draft: $draft,
            hint: TodoParseHint(text: "Call Ann", deadline: PreviewData.day(2).dateString, dueTime: "15:00"), now: PreviewData.now
        )
    }
    .preferredColorScheme(.light)
}

#Preview("To-dos, empty") {
    @Previewable @State var draft = ""
    NavigationStack { TodosContent(board: TodoBoard(), offline: true, draft: $draft) }
        .preferredColorScheme(.dark)
}
