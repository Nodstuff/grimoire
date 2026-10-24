import Foundation

/// A shelf of the Books screen: everything, a reading state, the month's
/// additions, the black-and-white covers, or one series.
public enum BookShelfFilter: Hashable, Sendable {
    case all, reading, unread, finished, addedThisMonth, blackAndWhite
    case series(String)

    public var title: String {
        switch self {
        case .all: "All books"
        case .reading: "Reading"
        case .unread: "Unread"
        case .finished: "Finished"
        case .addedThisMonth: "Added this month"
        case .blackAndWhite: "Black-and-white covers"
        case let .series(s): s
        }
    }

    /// The fixed shelves, in the order the menu and the sidebar list them.
    public static let shelves: [BookShelfFilter] = [.reading, .unread, .finished, .addedThisMonth, .blackAndWhite]
}

/// The library as the Books screen shows it (pure: the screen, its tests
/// and the iPad pane all read the same answers).
public struct BookShelf: Sendable, Hashable {
    public var books: [Book]
    public var progress: [String: BookProgress]
    public var reading: [ReadingBook]
    public var ereaderCount: Int

    public init(books: [Book] = [], progress: [String: BookProgress] = [:], reading: [ReadingBook] = [], ereaderCount: Int = 0) {
        self.books = books
        self.progress = progress
        self.reading = reading
        self.ereaderCount = ereaderCount
    }

    public func status(_ id: String) -> ProgressStatus { progress[id]?.status ?? .unread }

    /// "128 books · 3 reading · on 2 e-readers"
    public var summary: String {
        var parts = [books.count == 1 ? "1 book" : "\(books.count) books"]
        let n = readingCount
        if n > 0 { parts.append("\(n) reading") }
        if ereaderCount > 0 { parts.append(ereaderCount == 1 ? "on 1 e-reader" : "on \(ereaderCount) e-readers") }
        return parts.joined(separator: " · ")
    }

    public var readingCount: Int {
        max(reading.count, books.filter { status($0.id) == .reading }.count)
    }

    /// Series with their sizes, biggest first (then by name).
    public var series: [(name: String, count: Int)] {
        var counts: [String: Int] = [:]
        for b in books { if let s = b.seriesName { counts[s, default: 0] += 1 } }
        return counts.map { (name: $0.key, count: $0.value) }
            .sorted { $0.count != $1.count ? $0.count > $1.count : $0.name.localizedStandardCompare($1.name) == .orderedAscending }
    }

    public func count(_ f: BookShelfFilter, now: Date = .now, calendar: Calendar = .current) -> Int {
        books.count { matches($0, f, now: now, calendar: calendar) }
    }

    public func matches(_ b: Book, _ f: BookShelfFilter, now: Date = .now, calendar: Calendar = .current) -> Bool {
        switch f {
        case .all: true
        case .reading: status(b.id) == .reading
        case .unread: status(b.id) == .unread
        case .finished: status(b.id) == .finished
        case .addedThisMonth: b.addedAt.map { calendar.isDate($0, equalTo: now, toGranularity: .month) } ?? false
        case .blackAndWhite: b.wantsColourCover
        case let .series(s): b.seriesName == s
        }
    }

    /// The grid: the shelf's books filtered by a query (title, author or
    /// series contains it), ordered by series then number then title (books
    /// without a series by title).
    public func shown(_ f: BookShelfFilter, query: String = "", now: Date = .now, calendar: Calendar = .current) -> [Book] {
        let q = query.trimmingCharacters(in: .whitespacesAndNewlines)
        return books
            .filter { matches($0, f, now: now, calendar: calendar) }
            .filter { q.isEmpty || Self.matchesQuery($0, q) }
            .sorted(by: Self.seriesThenTitle)
    }

    public static func matchesQuery(_ b: Book, _ q: String) -> Bool {
        [b.title, b.authorName ?? "", b.seriesName ?? ""].contains { $0.localizedStandardContains(q) }
    }

    public static func seriesThenTitle(_ a: Book, _ b: Book) -> Bool {
        let sa = a.seriesName ?? a.title, sb = b.seriesName ?? b.title
        let c = sa.localizedStandardCompare(sb)
        if c != .orderedSame { return c == .orderedAscending }
        let ia = a.seriesIndex ?? .infinity, ib = b.seriesIndex ?? .infinity
        if ia != ib { return ia < ib }
        return a.title.localizedStandardCompare(b.title) == .orderedAscending
    }

    /// The books being read, newest progress first: the server's list,
    /// else the shelf's own reading rows.
    public var continueReading: [(book: Book, progress: BookProgress)] {
        if !reading.isEmpty {
            // the shelf's copy of the book wins (a rename since the list came)
            let byID = Dictionary(books.map { ($0.id, $0) }, uniquingKeysWith: { a, _ in a })
            return reading.map { (book: byID[$0.book.id] ?? $0.book, progress: $0.progress) }
        }
        return books.compactMap { b in progress[b.id].flatMap { $0.status == .reading ? (book: b, progress: $0) : nil } }
            .sorted { ($0.progress.updatedAt ?? .distantPast) > ($1.progress.updatedAt ?? .distantPast) }
    }

    /// Added in the last week and not opened yet: the "NEW" badge.
    public func isNew(_ b: Book, now: Date = .now) -> Bool {
        guard status(b.id) == .unread, let at = b.addedAt else { return false }
        return now.timeIntervalSince(at) < 7 * 86_400
    }

    /// The grid's second line: "12% · Tue", "Finished", "Added today", "Unread".
    public func stateLine(_ b: Book, now: Date = .now, calendar: Calendar = .current) -> String {
        switch status(b.id) {
        case .finished: return "Finished"
        case .reading:
            let p = progress[b.id]
            let pct = BookFormat.percent(p?.percentage ?? 0)
            guard let at = p?.updatedAt else { return pct }
            return "\(pct) · \(BookFormat.shortWhen(at, now: now, calendar: calendar))"
        case .unread:
            if let at = b.addedAt, isNew(b, now: now) {
                return calendar.isDate(at, inSameDayAs: now) ? "Added today" : "Added \(BookFormat.shortWhen(at, now: now, calendar: calendar))"
            }
            return "Unread"
        }
    }
}

/// Labels the Books screens share.
public enum BookFormat {
    /// "64%" from a 0–100 percentage.
    public static func percent(_ p: Double) -> String {
        "\(Int(max(0, min(100, p)).rounded()))%"
    }

    /// "Kindle · 2 h ago"
    public static func deviceLine(_ p: BookProgress, now: Date = .now) -> String {
        let device = p.device.isEmpty ? "Another device" : p.device
        guard let at = p.updatedAt else { return device }
        return "\(device) · \(ago(at, now: now))"
    }

    /// "just now", "5 min ago", "2 h ago", "yesterday", "3 days ago", "12 Aug".
    public static func ago(_ at: Date, now: Date = .now, calendar: Calendar = .current) -> String {
        let s = now.timeIntervalSince(at)
        if s < 60 { return "just now" }
        if s < 3600 { return "\(Int(s / 60)) min ago" }
        if s < 86_400, calendar.isDate(at, inSameDayAs: now) || s < 6 * 3600 { return "\(Int(s / 3600)) h ago" }
        if let y = calendar.date(byAdding: .day, value: -1, to: now), calendar.isDate(at, inSameDayAs: y) { return "yesterday" }
        if s < 7 * 86_400 { return "\(max(2, Int(s / 86_400))) days ago" }
        return at.formatted(.dateTime.day().month(.abbreviated))
    }

    /// "Tue" within the week, "today", "yesterday", else "12 Aug".
    public static func shortWhen(_ at: Date, now: Date = .now, calendar: Calendar = .current) -> String {
        if calendar.isDate(at, inSameDayAs: now) { return "today" }
        if let y = calendar.date(byAdding: .day, value: -1, to: now), calendar.isDate(at, inSameDayAs: y) { return "yesterday" }
        if now.timeIntervalSince(at) < 6 * 86_400 { return at.formatted(.dateTime.weekday(.abbreviated)) }
        return at.formatted(.dateTime.day().month(.abbreviated))
    }

    /// "THE HORUS HERESY · BOOK 14" (uppercased by the view), or nil.
    public static func seriesLine(_ b: Book) -> String? {
        guard let s = b.seriesName else { return nil }
        guard let i = b.seriesIndex else { return s }
        return "\(s) · Book \(number(i))"
    }

    public static func number(_ n: Double) -> String {
        n.rounded() == n ? String(Int(n)) : String(n)
    }

    /// "1.1 MB"
    public static func size(_ bytes: Int64) -> String {
        ByteCountFormatter.string(fromByteCount: bytes, countStyle: .file)
    }
}
