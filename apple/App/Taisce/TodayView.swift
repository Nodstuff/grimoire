import TaisceKit
import SwiftUI

/// Due & overdue from the server's read-only `GET /api/todo/due` (overdue by
/// time), plus today's list from the cached To-do doc. Never calls
/// `GET /api/todo`, which can carry items forward on the server. Offline,
/// both lists come from the cache.
struct TodayView: View {
    @Environment(AppModel.self) private var model
    @Binding var selection: SidebarItem?
    @State private var due: [Row] = []
    @State private var today: [Row] = []
    @State private var offline = false

    struct Row: Identifiable, Hashable {
        var id: String
        var text: String
        var done: Bool
        var due: Due?
        var note: String?
    }

    var body: some View {
        List {
            if !due.isEmpty {
                Section(offline ? "Due & overdue (offline)" : "Due & overdue") {
                    ForEach(due) { TodoRow(row: $0) }
                }
            }
            Section("Today") {
                if today.isEmpty {
                    Text("Nothing scheduled.").foregroundStyle(.secondary)
                } else {
                    ForEach(today) { TodoRow(row: $0) }
                }
            }
        }
        .navigationTitle("Today")
        .refreshable { await load() }
        .task { await load() }
    }

    private func load() async {
        let todayString = Due.today.dateString
        let cached = (try? await model.cache?.todos()) ?? []
        today = cached.filter { $0.date == todayString }.map {
            Row(id: "\($0.date)/\($0.position)", text: $0.text, done: $0.mark == "x", due: $0.due, note: $0.note)
        }
        if let api = model.api, let list = try? await api.todoDue(until: todayString) {
            offline = false
            due = list.items.map { Row(id: $0.id, text: $0.text, done: false, due: $0.due, note: $0.note) }
        } else {
            offline = true
            due = TodoParser.dueOrOverdue(cached).map {
                Row(id: "\($0.date)/\($0.position)", text: $0.text, done: false, due: $0.due, note: $0.note)
            }
        }
    }
}

private struct TodoRow: View {
    let row: TodayView.Row
    var text: String { row.text }
    var done: Bool { row.done }
    var due: Due? { row.due }
    var note: String? { row.note }

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
