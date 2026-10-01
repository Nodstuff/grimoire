import Foundation
import TaisceKit

/// One open to-do, from the server's due list or the cached To-do doc.
/// `date` + `itemID` address it for writes (the server's item id scheme).
struct TodoEntry: Identifiable, Hashable, Sendable {
    var date: String
    var itemID: String
    var text: String
    var due: Due?
    var overdue: Bool
    var note: String?

    var id: String { "\(date)/\(itemID)" }

    init(date: String, itemID: String, text: String, due: Due? = nil, overdue: Bool = false, note: String? = nil) {
        self.date = date
        self.itemID = itemID
        self.text = text
        self.due = due
        self.overdue = overdue
        self.note = note
    }

    init(_ item: TodoDueList.Item) {
        self.init(date: item.date, itemID: item.itemID, text: item.text, due: item.due, overdue: item.overdue, note: item.note)
    }

    /// From the cache. The deadline shows as a wall time in `timeZone`;
    /// overdue is the device's call (by the instant, or after an all-day date).
    init(_ r: TodoRecord, now: Date = .now, timeZone: TimeZone = .current) {
        self.init(
            date: r.date, itemID: Self.itemID(position: r.position, text: r.text), text: r.text,
            due: r.deadlineValue?.due(in: timeZone), overdue: r.isOverdue(now: now, in: timeZone), note: r.note
        )
    }

    /// A local wall-time deadline's overdue rule (the device decides).
    static func isOverdue(_ due: Due, now: Date, timeZone: TimeZone) -> Bool {
        if due.hasTime { return (due.alertDate(in: timeZone) ?? .distantFuture) < now }
        return due.dateString < Due.today(now: now, in: timeZone).dateString
    }

    /// `<index among the day's items>-<fnv1a(text)>` (crates/daemon/src/todo.rs).
    /// The server falls back to the hash when the index has shifted.
    static func itemID(position: Int, text: String) -> String {
        var h: UInt32 = 0x811c_9dc5
        for b in text.trimmingCharacters(in: .whitespaces).utf8 {
            h ^= UInt32(b)
            h = h &* 0x0100_0193
        }
        return "\(position)-" + String(format: "%08x", h)
    }
}

/// How a deadline reads on a card: the ring's tone and the subtitle.
struct DueLabel: Hashable, Sendable {
    enum Tone: Hashable, Sendable { case overdue, today, later, none }

    var tone: Tone
    var text: String?

    static func make(_ e: TodoEntry, now: Date = .now, calendar: Calendar = .current) -> DueLabel {
        guard let due = e.due else { return DueLabel(tone: .none, text: nil) }
        let time = due.hasTime ? String(format: "%02d:%02d", due.hour ?? 0, due.minute ?? 0) : nil
        let today = Due.today(now: now, in: calendar.timeZone).dateString
        let days = dayDistance(from: today, to: due.dateString, calendar: calendar)
        if e.overdue {
            let when: String = switch days {
            case 0: time ?? "today"
            case -1: "yesterday"
            case let d where d > -7: "\(-d) days ago"
            default: shortDate(due, calendar: calendar)
            }
            return DueLabel(tone: .overdue, text: "Overdue · \(when)")
        }
        switch days {
        case 0:
            return DueLabel(tone: .today, text: time.map { "Today · \($0)" } ?? "Today")
        case 1:
            return DueLabel(tone: .later, text: time.map { "Tomorrow · \($0)" } ?? "Tomorrow")
        default:
            let d = shortDate(due, calendar: calendar, weekday: days > 1 && days < 7)
            return DueLabel(tone: .later, text: time.map { "\(d) · \($0)" } ?? d)
        }
    }

    /// Whole days from `a` to `b` (`YYYY-MM-DD`), negative when `b` is earlier.
    static func dayDistance(from a: String, to b: String, calendar: Calendar) -> Int {
        guard let da = date(a, calendar), let db = date(b, calendar) else { return 0 }
        return calendar.dateComponents([.day], from: da, to: db).day ?? 0
    }

    static func date(_ s: String, _ calendar: Calendar) -> Date? {
        guard let d = Due(s) else { return nil }
        return calendar.date(from: DateComponents(year: d.year, month: d.month, day: d.day))
    }

    static func shortDate(_ due: Due, calendar: Calendar, weekday: Bool = false) -> String {
        guard let date = date(due.dateString, calendar) else { return due.dateString }
        let f = DateFormatter()
        f.calendar = calendar
        f.timeZone = calendar.timeZone
        f.setLocalizedDateFormatFromTemplate(weekday ? "EEE" : "EEE d MMM")
        return f.string(from: date).replacingOccurrences(of: ",", with: "")
    }
}

/// The To-dos tab: Overdue / Today / Upcoming, each soonest first.
struct TodoBoard: Hashable, Sendable {
    var overdue: [TodoEntry] = []
    var today: [TodoEntry] = []
    var upcoming: [TodoEntry] = []

    var isEmpty: Bool { overdue.isEmpty && today.isEmpty && upcoming.isEmpty }
    var count: Int { overdue.count + today.count + upcoming.count }

    /// `dated` comes from the due list; `undated` are today's items without
    /// a deadline (they belong under Today). Duplicates (same id) collapse.
    static func build(dated: [TodoEntry], undated: [TodoEntry] = [], now: Date = .now, timeZone: TimeZone = .current) -> TodoBoard {
        let today = Due.today(now: now, in: timeZone).dateString
        var board = TodoBoard()
        var seen: Set<String> = []
        for e in dated + undated where seen.insert(e.id).inserted {
            if e.overdue {
                board.overdue.append(e)
            } else if let due = e.due, due.dateString > today {
                board.upcoming.append(e)
            } else {
                board.today.append(e)
            }
        }
        let order: (TodoEntry, TodoEntry) -> Bool = { a, b in
            switch (a.due, b.due) {
            case let (x?, y?): x < y
            case (.some, nil): true
            case (nil, .some): false
            case (nil, nil): a.id < b.id
            }
        }
        board.overdue.sort(by: order)
        board.today.sort(by: order)
        board.upcoming.sort(by: order)
        return board
    }

    /// The board to show, or nil for "loading": an empty board before the
    /// first sync has landed is not yet "nothing due". Once the connection
    /// has failed (waiting), empty means empty.
    static func shown(_ board: TodoBoard?, hasSynced: Bool, status: SyncStatus) -> TodoBoard? {
        guard let board else { return nil }
        if board.isEmpty, !hasSynced, status == .idle || status == .catchingUp || status == .live { return nil }
        return board
    }

    /// Today's "DUE" card list: overdue, then due today.
    var due: [TodoEntry] { overdue + today.filter { $0.due != nil } }
}

/// Snooze targets offered on a left swipe.
enum Snooze: CaseIterable, Hashable, Sendable {
    case oneHour, tomorrowMorning

    var title: String {
        switch self {
        case .oneHour: "1 hour"
        case .tomorrowMorning: "Tomorrow 09:00"
        }
    }

    func deadline(now: Date = .now, calendar: Calendar = .current) -> Due? {
        let target: Date? = switch self {
        case .oneHour: now.addingTimeInterval(3600)
        case .tomorrowMorning: calendar.date(byAdding: .day, value: 1, to: now)
            .flatMap { calendar.date(bySettingHour: Due.defaultAlertHour, minute: 0, second: 0, of: $0) }
        }
        guard let target else { return nil }
        let c = calendar.dateComponents([.year, .month, .day, .hour, .minute], from: target)
        guard let y = c.year, let m = c.month, let d = c.day else { return nil }
        return Due(year: y, month: m, day: d, hour: c.hour ?? Due.defaultAlertHour, minute: c.minute ?? 0)
    }
}
