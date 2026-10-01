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
///
/// Deadlines: `deadline` (`YYYY-MM-DD`) for an all-day item, `due_at` (RFC
/// 3339 UTC) for a timed one, `legacy_time` on a pre-UTC one. Overdue is
/// the device's call (`deadlineValue?.isOverdue()`), never the server's.
/// Pre-UTC servers sent `deadline` + `due_time` + `alert_at` + `overdue`;
/// those still decode (the time is read as UTC, as the server reads it).
public struct TodoItem: Codable, Sendable, Hashable, Identifiable {
    public var id: String
    public var text: String
    public var done: Bool
    public var carried: Bool
    public var carriedFrom: String?
    /// all-day `YYYY-MM-DD` (pre-UTC servers: the day of a timed one too)
    public var deadline: String?
    /// timed: the instant (RFC 3339 UTC)
    public var dueAt: String?
    public var legacyTime: Bool
    /// pre-UTC servers only: `HH:MM`
    public var dueTime: String?
    public var note: String?
    public var dueSoon: Bool

    public init(
        id: String, text: String, done: Bool, carried: Bool = false, carriedFrom: String? = nil,
        deadline: String? = nil, dueAt: String? = nil, legacyTime: Bool = false, dueTime: String? = nil,
        note: String? = nil, dueSoon: Bool = false
    ) {
        self.id = id
        self.text = text
        self.done = done
        self.carried = carried
        self.carriedFrom = carriedFrom
        self.deadline = deadline
        self.dueAt = dueAt
        self.legacyTime = legacyTime
        self.dueTime = dueTime
        self.note = note
        self.dueSoon = dueSoon
    }

    public var deadlineValue: Deadline? {
        Deadline.fromWire(deadline: deadline, dueAt: dueAt, legacyTime: legacyTime, dueTime: dueTime)
    }

    /// The deadline as a wall-clock date/time in the device's zone.
    public var due: Due? { deadlineValue?.due() }

    /// Past due on this device, now.
    public var overdue: Bool { !done && (deadlineValue?.isOverdue() ?? false) }

    enum CodingKeys: String, CodingKey {
        case id, text, done, carried, deadline, note
        case carriedFrom = "carried_from"
        case dueSoon = "due_soon"
        case dueAt = "due_at"
        case legacyTime = "legacy_time"
        case dueTime = "due_time"
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
        dueAt = try c.decodeIfPresent(String.self, forKey: .dueAt)
        legacyTime = try c.decodeIfPresent(Bool.self, forKey: .legacyTime) ?? false
        dueTime = try c.decodeIfPresent(String.self, forKey: .dueTime)
        note = try c.decodeIfPresent(String.self, forKey: .note)
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
/// soonest first. Read-only (never carries forward), so safe to poll. The
/// server compares in UTC; `dueToday(now:)` gives the device's view.
public struct TodoDueList: Codable, Sendable, Hashable {
    public struct Item: Codable, Sendable, Hashable, Identifiable {
        /// the day the item is scheduled under; with `itemID`, its address for writes
        public var date: String
        public var itemID: String
        public var text: String
        /// all-day `YYYY-MM-DD` (pre-UTC servers: also a timed one's day)
        public var allDay: String?
        /// timed: RFC 3339 UTC
        public var dueAt: String?
        public var legacyTime: Bool
        /// pre-UTC servers only
        public var legacyDueTime: String?
        public var note: String?
        public var carriedFrom: String?
        /// Lists read without a `workspace` (every list): which one it is on.
        public var docID: DocID?
        public var workspaceID: WorkspaceID?

        public var id: String { "\(date)/\(itemID)" }

        public var deadlineValue: Deadline? {
            Deadline.fromWire(deadline: allDay, dueAt: dueAt, legacyTime: legacyTime, dueTime: legacyDueTime)
        }

        /// The deadline as a wall-clock date/time in the device's zone.
        public var due: Due? { deadlineValue?.due() }
        /// The deadline's day in the device's zone.
        public var deadline: String { deadlineValue?.day() ?? allDay ?? "" }
        /// `HH:MM` in the device's zone, for a timed deadline.
        public var dueTime: String? {
            guard let d = due, let h = d.hour, let m = d.minute else { return nil }
            return String(format: "%02d:%02d", h, m)
        }
        /// Past due on this device, now (the server no longer says).
        public var overdue: Bool { deadlineValue?.isOverdue() ?? false }
        /// When to notify: the instant, or 09:00 local on an all-day date.
        public var alertDate: Date? { deadlineValue?.alertDate() }

        enum CodingKeys: String, CodingKey {
            case date, text, note
            case itemID = "id"
            case allDay = "deadline"
            case dueAt = "due_at"
            case legacyTime = "legacy_time"
            case legacyDueTime = "due_time"
            case carriedFrom = "carried_from"
            case docID = "doc_id"
            case workspaceID = "workspace_id"
        }

        public init(from decoder: any Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            date = try c.decode(String.self, forKey: .date)
            itemID = try c.decode(String.self, forKey: .itemID)
            text = try c.decode(String.self, forKey: .text)
            allDay = try c.decodeIfPresent(String.self, forKey: .allDay)
            dueAt = try c.decodeIfPresent(String.self, forKey: .dueAt)
            legacyTime = try c.decodeIfPresent(Bool.self, forKey: .legacyTime) ?? false
            legacyDueTime = try c.decodeIfPresent(String.self, forKey: .legacyDueTime)
            note = try c.decodeIfPresent(String.self, forKey: .note)
            carriedFrom = try c.decodeIfPresent(String.self, forKey: .carriedFrom)
            docID = try c.decodeIfPresent(DocID.self, forKey: .docID)
            workspaceID = try c.decodeIfPresent(WorkspaceID.self, forKey: .workspaceID)
        }
    }

    /// Due by the end of the device's today, or overdue: soonest first.
    public func dueToday(now: Date = .now, in timeZone: TimeZone = .current) -> [Item] {
        items
            .filter { $0.deadlineValue?.isDueToday(now: now, in: timeZone) ?? false }
            .sorted { ($0.deadlineValue?.sortDate(in: timeZone) ?? .distantFuture) < ($1.deadlineValue?.sortDate(in: timeZone) ?? .distantFuture) }
    }

    public var docID: DocID?
    public var epoch: Int
    public var now: String
    /// echo of the `until` asked for; nil when none was given
    public var until: String?
    public var defaultAlertTime: String
    public var items: [Item]

    enum CodingKeys: String, CodingKey {
        case epoch, now, until, items
        case docID = "doc_id"
        case defaultAlertTime = "default_alert_time"
    }
}
