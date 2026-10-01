import Foundation

/// A to-do deadline as the server stores it, with the device owning local
/// time. The server never reads a local clock:
///
/// - `.allDay("2026-10-03")`: a floating date (` · due 2026-10-03`). It is
///   due all day wherever the device is; it becomes overdue at local
///   midnight after it, and alerts at 09:00 local on the day.
/// - `.at(instant)`: a UTC instant (` · due 2026-10-03T14:00Z`, API
///   `due_at`). Shown in the device's zone; overdue and alerts by the instant.
///
/// A pre-UTC ` · due 2026-10-03 14:00` is read as UTC, as the server reads
/// it (`legacy_time`); `isLegacy` marks it.
public enum Deadline: Sendable, Hashable {
    case allDay(String)
    case at(Date, isLegacy: Bool = false)

    public static let alertHour = 9

    /// The token after ` · due `: `YYYY-MM-DD`, `YYYY-MM-DDTHH:MMZ`, RFC 3339
    /// with a zone, or the legacy `YYYY-MM-DD HH:MM` (read as UTC).
    public init?(stored text: String) {
        let s = text.trimmingCharacters(in: .whitespaces)
        if let d = Due(s), !d.hasTime {
            self = .allDay(d.dateString)
        } else if let d = Due(s), let h = d.hour, let m = d.minute,
                  let t = Self.utc.date(from: DateComponents(year: d.year, month: d.month, day: d.day, hour: h, minute: m)) {
            self = .at(t, isLegacy: true)
        } else if let t = Self.instant(s) {
            self = .at(t)
        } else {
            return nil
        }
    }

    /// The user picked a day and, optionally, a time on this device: a time
    /// is that wall time in `timeZone`, stored as UTC. In a DST overlap (the
    /// hour that happens twice) the first occurrence wins; in a gap (the hour
    /// that never happens) the time moves forward by the gap.
    public static func local(_ day: Due, in timeZone: TimeZone = .current) -> Deadline? {
        guard let hour = day.hour, let minute = day.minute else { return .allDay(day.dateString) }
        var cal = Calendar(identifier: .gregorian)
        cal.timeZone = timeZone
        // from just before that local midnight, the first matching wall time
        guard let midnight = cal.date(from: DateComponents(year: day.year, month: day.month, day: day.day)),
              let t = cal.nextDate(
                after: midnight.addingTimeInterval(-1), matching: DateComponents(hour: hour, minute: minute, second: 0),
                matchingPolicy: .nextTimePreservingSmallerComponents, repeatedTimePolicy: .first, direction: .forward
              )
        else { return nil }
        return .at(t)
    }

    /// From an API answer: `due_at` wins; a pre-UTC `deadline` + `due_time`
    /// is a wall time with no zone, read as UTC (as the server reads it).
    static func fromWire(deadline: String?, dueAt: String?, legacyTime: Bool, dueTime: String?) -> Deadline? {
        if let dueAt, let t = instant(dueAt) { return .at(t, isLegacy: legacyTime) }
        if let deadline, let dueTime, case let .at(t, _)? = Deadline(stored: "\(deadline) \(dueTime)") { return .at(t, isLegacy: true) }
        return deadline.flatMap { Deadline(stored: $0) }
    }

    // MARK: reading it on this device

    /// The calendar day it falls on in `timeZone` (an all-day date is the
    /// same everywhere).
    public func day(in timeZone: TimeZone = .current) -> String {
        switch self {
        case let .allDay(d): d
        case let .at(t, _): Self.calendar(timeZone).dateString(t)
        }
    }

    /// The wall-clock view for display (and the older `Due` APIs).
    public func due(in timeZone: TimeZone = .current) -> Due? {
        switch self {
        case let .allDay(d): return Due(d)
        case let .at(t, _):
            let c = Self.calendar(timeZone).dateComponents([.year, .month, .day, .hour, .minute], from: t)
            guard let y = c.year, let mo = c.month, let d = c.day else { return nil }
            return Due(year: y, month: mo, day: d, hour: c.hour ?? 0, minute: c.minute ?? 0)
        }
    }

    /// Past due: a timed one after its instant; an all-day one once its date
    /// has ended in the device's zone (local midnight after it).
    public func isOverdue(now: Date = .now, in timeZone: TimeZone = .current) -> Bool {
        switch self {
        case let .allDay(d): return d < Self.calendar(timeZone).dateString(now)
        case let .at(t, _): return now > t
        }
    }

    /// Due by the end of the device's today (or already past).
    public func isDueToday(now: Date = .now, in timeZone: TimeZone = .current) -> Bool {
        day(in: timeZone) <= Self.calendar(timeZone).dateString(now)
    }

    /// When the reminder fires: the instant, or 09:00 local on an all-day date.
    public func alertDate(in timeZone: TimeZone = .current) -> Date? {
        switch self {
        case let .at(t, _): return t
        case let .allDay(d):
            guard let day = Due(d) else { return nil }
            return Self.calendar(timeZone).date(from: DateComponents(year: day.year, month: day.month, day: day.day, hour: Self.alertHour))
        }
    }

    /// Sort key: the instant, or the start of the all-day date in `timeZone`.
    public func sortDate(in timeZone: TimeZone = .current) -> Date {
        switch self {
        case let .at(t, _): return t
        case let .allDay(d):
            let day = Due(d)
            return day.flatMap { Self.calendar(timeZone).date(from: DateComponents(year: $0.year, month: $0.month, day: $0.day)) } ?? .distantFuture
        }
    }

    // MARK: the wire

    /// What `POST /api/todo/deadline` takes: `deadline` (all-day) or `due_at`.
    public var wire: (deadline: String?, dueAt: String?) {
        switch self {
        case let .allDay(d): (d, nil)
        case let .at(t, _): (nil, Self.rfc3339(t))
        }
    }

    /// The doc-text form the server writes.
    public var stored: String {
        switch self {
        case let .allDay(d): d
        case let .at(t, _): Self.storedFormatter.string(from: t)
        }
    }

    static func rfc3339(_ t: Date) -> String { rfcFormatter.string(from: t) }

    static func instant(_ s: String) -> Date? {
        if let t = rfcFormatter.date(from: s) ?? rfcFractional.date(from: s) { return t.truncatedToMinute }
        return storedFormatter.date(from: s.uppercased())
    }

    static let utc: Calendar = calendar(TimeZone(identifier: "UTC") ?? .gmt)

    static func calendar(_ tz: TimeZone) -> Calendar {
        var c = Calendar(identifier: .gregorian)
        c.timeZone = tz
        return c
    }

    // ISO8601DateFormatter is thread-safe for formatting and parsing once configured
    nonisolated(unsafe) static let rfcFormatter: ISO8601DateFormatter = {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime]
        return f
    }()

    nonisolated(unsafe) static let rfcFractional: ISO8601DateFormatter = {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return f
    }()

    /// `2026-10-03T14:00Z`
    nonisolated(unsafe) static let storedFormatter: DateFormatter = {
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.timeZone = TimeZone(identifier: "UTC")
        f.dateFormat = "yyyy-MM-dd'T'HH:mm'Z'"
        return f
    }()
}

extension Date {
    var truncatedToMinute: Date {
        Date(timeIntervalSinceReferenceDate: (timeIntervalSinceReferenceDate / 60).rounded(.down) * 60)
    }
}

extension Calendar {
    /// `YYYY-MM-DD` of `date` in this calendar's zone.
    func dateString(_ date: Date) -> String {
        let c = dateComponents([.year, .month, .day], from: date)
        return String(format: "%04d-%02d-%02d", c.year ?? 1970, c.month ?? 1, c.day ?? 1)
    }
}

/// The device's clock, sent with every to-do call: the server has no idea
/// what day it is for you. `utc_offset` only matters where the server reads
/// a typed time ("due fri 3pm"); deadlines the app sets are UTC already.
public struct TodoClock: Sendable, Hashable, Encodable {
    public var today: String
    public var utcOffset: String
    /// The zone `today` was read in (not sent): the local day of a timed
    /// deadline is taken in it.
    public var timeZone: TimeZone

    /// A fixed clock; the zone is the offset's (`+01:00` → UTC+1).
    public init(today: String, utcOffset: String) {
        self.today = today
        self.utcOffset = utcOffset
        timeZone = Self.zone(utcOffset) ?? .gmt
    }

    static func zone(_ offset: String) -> TimeZone? {
        let parts = offset.dropFirst().split(separator: ":")
        guard let sign = offset.first, sign == "+" || sign == "-", parts.count == 2,
              let h = Int(parts[0]), let m = Int(parts[1])
        else { return nil }
        return TimeZone(secondsFromGMT: (sign == "-" ? -1 : 1) * (h * 3600 + m * 60))
    }

    public init(now: Date = .now, timeZone: TimeZone = .current) {
        self.timeZone = timeZone
        today = Deadline.calendar(timeZone).dateString(now)
        let secs = timeZone.secondsFromGMT(for: now)
        let sign = secs < 0 ? "-" : "+"
        utcOffset = String(format: "%@%02d:%02d", sign, abs(secs) / 3600, (abs(secs) % 3600) / 60)
    }

    enum CodingKeys: String, CodingKey {
        case today
        case utcOffset = "utc_offset"
    }
}
