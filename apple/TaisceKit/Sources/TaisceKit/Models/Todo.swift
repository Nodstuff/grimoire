import Foundation

/// A to-do deadline: `YYYY-MM-DD` with an optional `HH:MM`. A date-only
/// deadline alerts at 09:00 local time.
public struct Due: Sendable, Hashable, Comparable, CustomStringConvertible {
    public static let defaultAlertHour = 9

    public var year: Int
    public var month: Int
    public var day: Int
    public var hour: Int?
    public var minute: Int?

    public init(year: Int, month: Int, day: Int, hour: Int? = nil, minute: Int? = nil) {
        self.year = year
        self.month = month
        self.day = day
        self.hour = hour
        self.minute = minute
    }

    public var hasTime: Bool { hour != nil }

    /// Today's date (no time) in `timeZone`.
    public static func today(now: Date = .now, in timeZone: TimeZone = .current) -> Due {
        var cal = Calendar(identifier: .gregorian)
        cal.timeZone = timeZone
        let c = cal.dateComponents([.year, .month, .day], from: now)
        return Due(year: c.year ?? 1970, month: c.month ?? 1, day: c.day ?? 1)
    }

    public static var today: Due { today() }

    /// Parses `2026-09-12` or `2026-09-12 14:30`; nil for anything else
    /// (including impossible dates like 2026-02-31).
    public init?(_ text: String) {
        let parts = text.trimmingCharacters(in: .whitespaces).split(separator: " ", omittingEmptySubsequences: true)
        guard parts.count == 1 || parts.count == 2 else { return nil }
        let d = parts[0].split(separator: "-", omittingEmptySubsequences: false)
        guard d.count == 3, d[0].count == 4, d[1].count == 2, d[2].count == 2,
              let y = Int(d[0]), let m = Int(d[1]), let dd = Int(d[2]),
              Self.isValid(year: y, month: m, day: dd)
        else { return nil }
        year = y
        month = m
        day = dd
        if parts.count == 2 {
            let t = parts[1].split(separator: ":", omittingEmptySubsequences: false)
            guard t.count == 2, t[0].count == 2, t[1].count == 2,
                  let h = Int(t[0]), let mi = Int(t[1]), (0..<24).contains(h), (0..<60).contains(mi)
            else { return nil }
            hour = h
            minute = mi
        }
    }

    private static func isValid(year: Int, month: Int, day: Int) -> Bool {
        guard (1...12).contains(month), day >= 1 else { return false }
        var cal = Calendar(identifier: .gregorian)
        cal.timeZone = TimeZone(identifier: "UTC") ?? .current
        guard let date = cal.date(from: DateComponents(year: year, month: month, day: 1)),
              let range = cal.range(of: .day, in: .month, for: date)
        else { return false }
        return range.contains(day)
    }

    /// `YYYY-MM-DD` — the day part, as the server keys days.
    public var dateString: String {
        String(format: "%04d-%02d-%02d", year, month, day)
    }

    public var description: String {
        guard let hour, let minute else { return dateString }
        return dateString + String(format: " %02d:%02d", hour, minute)
    }

    /// When a reminder should fire: the given time, else 09:00, in `timeZone`.
    public func alertDate(in timeZone: TimeZone = .current) -> Date? {
        var cal = Calendar(identifier: .gregorian)
        cal.timeZone = timeZone
        return cal.date(from: DateComponents(
            year: year, month: month, day: day,
            hour: hour ?? Self.defaultAlertHour, minute: minute ?? 0
        ))
    }

    public static func < (a: Due, b: Due) -> Bool {
        (a.year, a.month, a.day, a.hour ?? Self.defaultAlertHour, a.minute ?? 0)
            < (b.year, b.month, b.day, b.hour ?? Self.defaultAlertHour, b.minute ?? 0)
    }
}

/// One item of `GET /api/todo` (`Item` in crates/daemon/src/todo.rs).
/// `id` is `<index>-<fnv1a(text)>`, stable only within its day.
public struct TodoItem: Codable, Sendable, Hashable, Identifiable {
    public var id: String
    public var text: String
    public var done: Bool
    public var carried: Bool
    public var carriedFrom: String?
    /// the deadline's day, `YYYY-MM-DD`
    public var deadline: String?
    /// `HH:MM` when the deadline has a time
    public var dueTime: String?
    /// `YYYY-MM-DDTHH:MM` local; the server's answer to "when to alert" (09:00 if no time)
    public var alertAt: String?
    public var note: String?
    public var overdue: Bool
    public var dueSoon: Bool

    public init(
        id: String, text: String, done: Bool, carried: Bool = false, carriedFrom: String? = nil,
        deadline: String? = nil, dueTime: String? = nil, alertAt: String? = nil,
        note: String? = nil, overdue: Bool = false, dueSoon: Bool = false
    ) {
        self.id = id
        self.text = text
        self.done = done
        self.carried = carried
        self.carriedFrom = carriedFrom
        self.deadline = deadline
        self.dueTime = dueTime
        self.alertAt = alertAt
        self.note = note
        self.overdue = overdue
        self.dueSoon = dueSoon
    }

    /// Deadline day + optional time. Also reads a combined `D HH:MM` deadline.
    public var due: Due? {
        guard let deadline else { return nil }
        return Due(dueTime.map { "\(deadline) \($0)" } ?? deadline)
    }

    enum CodingKeys: String, CodingKey {
        case id, text, done, carried, deadline, note, overdue
        case carriedFrom = "carried_from"
        case dueSoon = "due_soon"
        case dueTime = "due_time"
        case alertAt = "alert_at"
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        text = try c.decode(String.self, forKey: .text)
        done = try c.decode(Bool.self, forKey: .done)
        // skip_serializing_if on the server: absent means false / nil
        carried = try c.decodeIfPresent(Bool.self, forKey: .carried) ?? false
        carriedFrom = try c.decodeIfPresent(String.self, forKey: .carriedFrom)
        deadline = try c.decodeIfPresent(String.self, forKey: .deadline)
        dueTime = try c.decodeIfPresent(String.self, forKey: .dueTime)
        alertAt = try c.decodeIfPresent(String.self, forKey: .alertAt)
        note = try c.decodeIfPresent(String.self, forKey: .note)
        overdue = try c.decodeIfPresent(Bool.self, forKey: .overdue) ?? false
        dueSoon = try c.decodeIfPresent(Bool.self, forKey: .dueSoon) ?? false
    }
}

/// `GET /api/todo?date=` and every to-do write's answer.
public struct TodoDay: Codable, Sendable, Hashable {
    public var docID: DocID
    public var date: String
    public var today: String
    public var items: [TodoItem]
    public var carried: Int
    public var prevDate: String?
    public var epoch: Int
    /// set when a typed `due <when>` phrase looked like a date but didn't parse
    public var warning: String?

    public init(
        docID: DocID, date: String, today: String, items: [TodoItem], carried: Int = 0,
        prevDate: String? = nil, epoch: Int, warning: String? = nil
    ) {
        self.docID = docID
        self.date = date
        self.today = today
        self.items = items
        self.carried = carried
        self.prevDate = prevDate
        self.epoch = epoch
        self.warning = warning
    }

    enum CodingKeys: String, CodingKey {
        case date, today, items, carried, epoch, warning
        case docID = "doc_id"
        case prevDate = "prev_date"
    }
}

/// `GET /api/todo/due?until=`: open items with deadlines across all days,
/// soonest alert first. Read-only (never carries forward), so safe to poll.
public struct TodoDueList: Codable, Sendable, Hashable {
    public struct Item: Codable, Sendable, Hashable, Identifiable {
        /// the day the item is scheduled under; with `itemID`, its address for writes
        public var date: String
        public var itemID: String
        public var text: String
        public var deadline: String
        public var dueTime: String?
        /// local `YYYY-MM-DDTHH:MM`
        public var alertAt: String
        public var overdue: Bool
        public var note: String?
        public var carriedFrom: String?

        public var id: String { "\(date)/\(itemID)" }

        public var due: Due? { Due(dueTime.map { "\(deadline) \($0)" } ?? deadline) }

        enum CodingKeys: String, CodingKey {
            case date, text, deadline, overdue, note
            case itemID = "id"
            case dueTime = "due_time"
            case alertAt = "alert_at"
            case carriedFrom = "carried_from"
        }
    }

    public var docID: DocID?
    public var epoch: Int
    public var now: String
    public var until: String
    public var defaultAlertTime: String
    public var items: [Item]

    enum CodingKeys: String, CodingKey {
        case epoch, now, until, items
        case docID = "doc_id"
        case defaultAlertTime = "default_alert_time"
    }
}
