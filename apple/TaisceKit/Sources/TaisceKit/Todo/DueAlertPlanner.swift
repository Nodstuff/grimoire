import Foundation

/// One open to-do as the due-alert planner sees it, whichever to-do model it
/// came from: a timed item is an instant, an all-day item a calendar date
/// (it alerts at 09:00 wherever the device is that day).
public struct DueAlertInput: Sendable, Hashable, Identifiable {
    public enum Kind: Sendable, Hashable {
        case timed(Date)
        /// year, month, day
        case allDay(DateComponents)
    }

    /// the day the item is scheduled under; with `itemID`, its address for writes
    public var day: String
    public var itemID: String
    /// the to-do text
    public var title: String
    public var docTitle: String
    public var kind: Kind
    public var done: Bool

    /// `<day>/<itemID>`, as `TodoDueList.Item.id`
    public var id: String { "\(day)/\(itemID)" }

    public init(day: String, itemID: String, title: String, docTitle: String, kind: Kind, done: Bool = false) {
        self.day = day
        self.itemID = itemID
        self.title = title
        self.docTitle = docTitle
        self.kind = kind
        self.done = done
    }

    /// The adapter both to-do models go through. `dueAt` (RFC 3339, UTC) is a
    /// timed item's instant and wins; otherwise `deadline` (`YYYY-MM-DD`,
    /// or the older combined `YYYY-MM-DD HH:MM`) with an optional `dueTime`
    /// (`HH:MM`): a time there is local wall-clock time in `timeZone`.
    /// Nil when the item has no deadline.
    public init?(
        day: String, itemID: String, title: String, docTitle: String, done: Bool = false,
        dueAt: String? = nil, deadline: String? = nil, dueTime: String? = nil,
        timeZone: TimeZone = .current
    ) {
        let value: Deadline?
        if let dueAt, let instant = Self.parseInstant(dueAt) {
            value = .at(instant)
        } else if let deadline, let due = Due(dueTime.map { "\(deadline) \($0)" } ?? deadline) {
            // a wall time on this device: the first of a repeated hour, a
            // skipped hour moved forward (Deadline.local)
            value = Deadline.local(due, in: timeZone)
        } else {
            value = nil
        }
        self.init(day: day, itemID: itemID, title: title, docTitle: docTitle, done: done, deadline: value)
    }

    /// From the time model: an instant alerts at the instant, an all-day
    /// date at 09:00 local on it. Nil when there's no deadline.
    public init?(day: String, itemID: String, title: String, docTitle: String, done: Bool = false, deadline: Deadline?) {
        guard let deadline, let kind = Self.kind(deadline) else { return nil }
        self.init(day: day, itemID: itemID, title: title, docTitle: docTitle, kind: kind, done: done)
    }

    static func kind(_ d: Deadline) -> Kind? {
        switch d {
        case let .at(t, _):
            return .timed(t)
        case let .allDay(s):
            guard let due = Due(s) else { return nil }
            return .allDay(DateComponents(year: due.year, month: due.month, day: due.day))
        }
    }

    /// An item of `GET /api/todo`, scheduled under `day`.
    public init?(_ item: TodoItem, day: String, docTitle: String = TodoParser.todoDocTitle, timeZone: TimeZone = .current) {
        self.init(day: day, itemID: item.id, title: item.text, docTitle: docTitle, done: item.done, deadline: item.deadlineValue)
    }

    /// An item of `GET /api/todo/due`.
    public init?(_ item: TodoDueList.Item, docTitle: String = TodoParser.todoDocTitle, timeZone: TimeZone = .current) {
        self.init(day: item.date, itemID: item.itemID, title: item.text, docTitle: docTitle, deadline: item.deadlineValue)
    }

    /// A to-do parsed from the cached To-do doc. Carried (`>`) and done
    /// (`x`) rows count as done.
    public init?(_ record: TodoRecord, docTitle: String = TodoParser.todoDocTitle, timeZone: TimeZone = .current) {
        self.init(
            day: record.date, itemID: Self.serverItemID(position: record.position, text: record.text),
            title: record.text, docTitle: docTitle, done: !record.isOpen, deadline: record.deadlineValue
        )
    }

    /// The daemon's item id (`item_id` in crates/daemon/src/todo.rs):
    /// `<index among the day's items>-<fnv1a of the trimmed text, 8 hex>`.
    public static func serverItemID(position: Int, text: String) -> String {
        var h: UInt32 = 0x811c_9dc5
        for b in text.trimmingCharacters(in: .whitespaces).utf8 {
            h ^= UInt32(b)
            h = h &* 0x0100_0193
        }
        return "\(position)-" + String(format: "%08x", h)
    }

    static func parseInstant(_ s: String) -> Date? {
        let plain = ISO8601DateFormatter()
        if let d = plain.date(from: s) { return d }
        let fractional = ISO8601DateFormatter()
        fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return fractional.date(from: s)
    }
}

/// One local notification the app wants pending.
public struct PlannedAlert: Sendable, Hashable, Identifiable {
    /// `todo:<day>/<itemID>`
    public var identifier: String
    public var day: String
    public var itemID: String
    public var title: String
    public var body: String
    public var fireDate: Date
    /// the calendar trigger: a timed alert carries its time zone (a fixed
    /// instant); an all-day one has none, so it fires at 09:00 wall-clock
    /// wherever the device is
    public var components: DateComponents

    public var id: String { identifier }

    public init(identifier: String, day: String, itemID: String, title: String, body: String, fireDate: Date, components: DateComponents) {
        self.identifier = identifier
        self.day = day
        self.itemID = itemID
        self.title = title
        self.body = body
        self.fireDate = fireDate
        self.components = components
    }

    /// What decides whether a pending request must be replaced: everything
    /// the user sees and when it fires (not `fireDate`, which a request
    /// read back from the system only approximates).
    public var signature: String {
        let c = components
        let when = [c.year, c.month, c.day, c.hour, c.minute].map { $0.map(String.init) ?? "-" }.joined(separator: ",")
        return [identifier, title, body, when, c.timeZone?.identifier ?? "floating"].joined(separator: "|")
    }
}

/// A local answer to a notification action, held until a sync shows it, so
/// an offline Done or Snooze does not bring the old alert back.
public enum DueAlertOverride: Sendable, Hashable {
    case done
    case snoozed(Date)
}

/// Which notifications should be pending for the open to-dos. Pure.
public enum DueAlertPlanner {
    /// iOS keeps at most 64 pending local notifications per app.
    public static let cap = 64
    public static let identifierPrefix = "todo:"
    public static let allDayHour = Due.defaultAlertHour

    public static func identifier(day: String, itemID: String) -> String {
        "\(identifierPrefix)\(day)/\(itemID)"
    }

    /// Timed items fire at their instant, all-day ones at 09:00 local on
    /// their date. Done and past items are skipped; soonest first, at most
    /// `cap`.
    public static func plan(_ inputs: [DueAlertInput], timeZone: TimeZone, now: Date) -> [PlannedAlert] {
        var cal = Calendar(identifier: .gregorian)
        cal.timeZone = timeZone
        let time = DateFormatter()
        time.calendar = cal
        time.timeZone = timeZone
        time.locale = Locale(identifier: "en_US_POSIX")
        time.dateFormat = "HH:mm"

        var out: [PlannedAlert] = []
        var seen = Set<String>()
        for input in inputs where !input.done {
            let id = identifier(day: input.day, itemID: input.itemID)
            guard seen.insert(id).inserted else { continue }
            let fire: Date
            var components: DateComponents
            let due: String
            switch input.kind {
            case .timed(let instant):
                fire = instant
                components = cal.dateComponents([.year, .month, .day, .hour, .minute], from: instant)
                components.timeZone = timeZone
                due = "Due \(time.string(from: instant))"
            case .allDay(let date):
                components = DateComponents(year: date.year, month: date.month, day: date.day, hour: allDayHour, minute: 0)
                guard let at = cal.date(from: components) else { continue }
                fire = at
                due = "Due today"
            }
            guard fire > now else { continue }
            out.append(PlannedAlert(
                identifier: id, day: input.day, itemID: input.itemID, title: input.title,
                body: "\(due) · \(input.docTitle)", fireDate: fire, components: components
            ))
        }
        out.sort { ($0.fireDate, $0.identifier) < ($1.fireDate, $1.identifier) }
        return Array(out.prefix(cap))
    }

    /// Apply local overrides (keyed by `DueAlertInput.id`) to the inputs,
    /// and drop the ones the inputs have caught up with: a Done once the
    /// item is done or gone, a Snooze once the item is due then, gone, or
    /// the snooze time has passed.
    public static func applying(
        _ overrides: [String: DueAlertOverride], to inputs: [DueAlertInput], now: Date
    ) -> (inputs: [DueAlertInput], overrides: [String: DueAlertOverride]) {
        var kept: [String: DueAlertOverride] = [:]
        var out: [DueAlertInput] = []
        for var input in inputs {
            switch overrides[input.id] {
            case nil:
                out.append(input)
            case .done:
                if input.done { out.append(input) } else { kept[input.id] = .done }
            case .snoozed(let at):
                if input.done || at <= now || input.kind == .timed(at) {
                    out.append(input)
                } else {
                    kept[input.id] = .snoozed(at)
                    input.kind = .timed(at)
                    out.append(input)
                }
            }
        }
        return (out, kept)
    }

    /// Requests to remove (stale, or changed) and to add (new, or changed)
    /// so the pending `todo:` requests match `desired`. Other pending
    /// requests are left alone.
    public static func diff(pending: [PlannedAlert], desired: [PlannedAlert]) -> (remove: [String], add: [PlannedAlert]) {
        let want = Dictionary(desired.map { ($0.identifier, $0.signature) }, uniquingKeysWith: { a, _ in a })
        let have = Dictionary(
            pending.filter { $0.identifier.hasPrefix(identifierPrefix) }.map { ($0.identifier, $0.signature) },
            uniquingKeysWith: { a, _ in a }
        )
        let remove = have.filter { want[$0.key] != $0.value }.map(\.key).sorted()
        let add = desired.filter { have[$0.identifier] != $0.signature }
        return (remove, add)
    }
}
