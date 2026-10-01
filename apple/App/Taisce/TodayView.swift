import TaisceKit
import SwiftUI

/// Today's to-dos from the server (which carries open items forward), plus
/// everything due or overdue across days from the cached To-do doc — so the
/// view still works offline.
struct TodayView: View {
    @Environment(AppModel.self) private var model
    @Binding var selection: SidebarItem?
    @State private var day: TodoDay?
    @State private var due: [TodoRecord] = []
    @State private var offline = false

    var body: some View {
        List {
            if !due.isEmpty {
                Section("Due & overdue") {
                    ForEach(due, id: \.self) { item in
                        TodoRow(text: item.text, done: !item.isOpen, due: item.due, note: item.note)
                    }
                }
            }
            Section(offline ? "Today (offline)" : "Today") {
                if let day, !day.items.isEmpty {
                    ForEach(day.items) { item in
                        TodoRow(text: item.text, done: item.done, due: item.due, note: item.note)
                    }
                } else {
                    Text("Nothing scheduled.").foregroundStyle(.secondary)
                }
            }
        }
        .navigationTitle("Today")
        .refreshable { await load() }
        .task { await load() }
    }

    private func load() async {
        if let api = model.api, let d = try? await api.todoDay() {
            day = d
            offline = false
        } else {
            offline = true
        }
        if let cache = model.cache, let all = try? await cache.todos() {
            due = TodoParser.dueOrOverdue(all)
            if offline {
                let today = Due.today.dateString
                let items = all.filter { $0.date == today }.map {
                    TodoItem(id: "\($0.position)", text: $0.text, done: $0.mark == "x", deadline: $0.deadline, note: $0.note)
                }
                day = items.isEmpty ? nil : TodoDay(docID: "", date: today, today: today, items: items, carried: 0, prevDate: nil, epoch: 0)
            }
        }
    }
}

private struct TodoRow: View {
    let text: String
    let done: Bool
    let due: Due?
    let note: String?

    var body: some View {
        HStack(alignment: .firstTextBaseline) {
            Image(systemName: done ? "checkmark.circle.fill" : "circle")
                .foregroundStyle(done ? Color.accentColor : .secondary)
            VStack(alignment: .leading, spacing: 2) {
                Text(InlineMarkdown.attributed(text)).strikethrough(done)
                if let note { Text(note).font(.caption).foregroundStyle(.secondary) }
            }
            Spacer()
            if let due {
                Text(due.hasTime ? due.description : due.dateString)
                    .font(.caption.monospacedDigit())
                    .foregroundStyle(.secondary)
            }
        }
    }
}
