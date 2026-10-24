import Foundation
import Observation
import TaisceKit

/// Open review items per doc, for the Library's flag pills and the
/// sidebar's dots (`GET /api/queue`). Refreshed when the Library shows and
/// after a sync; a decision made here takes its doc's count down at once.
@MainActor @Observable
final class ReviewCounts {
    private(set) var counts: [DocID: Int] = [:]
    @ObservationIgnored private var last = Date.distantPast
    @ObservationIgnored private var inFlight = false
    /// how often a refresh really asks the server
    @ObservationIgnored var interval: TimeInterval = 20

    func count(_ doc: DocID) -> Int { counts[doc] ?? 0 }

    func refresh(_ api: APIClient?, force: Bool = false, now: Date = .now) async {
        guard let api, !inFlight, force || now.timeIntervalSince(last) >= interval else { return }
        inFlight = true
        defer { inFlight = false }
        last = now
        // an older daemon without the route (or offline): keep what we had
        guard let items = try? await api.reviewQueue() else { return }
        let fresh = ReviewQueueItem.counts(items)
        if fresh != counts { counts = fresh }
    }

    /// A doc's open items as its screen just fetched them.
    func set(_ doc: DocID, _ n: Int) {
        if count(doc) != n { counts[doc] = n == 0 ? nil : n }
    }

    func reset() {
        counts = [:]
        last = .distantPast
    }
}

/// What an inline change card shows for a review item: the verdict, who,
/// and the change as plain words (old struck, new underlined).
struct ChangeCardModel: Equatable {
    var verdict: Verdict
    var author: String
    var old: String
    var new: String
    var note: String?
    var acceptTitle: String
    var rejectTitle: String
    var date: Date?

    init(_ item: DocReviewItem) {
        verdict = item.kind == .parked ? .red : .yellow
        author = item.proposer.isEmpty ? "someone" : item.proposer
        old = ReviewText.plain(item.before)
        new = ReviewText.plain(item.after)
        date = item.date
        switch (item.kind, item.opType) {
        case (.parked, "delete"):
            acceptTitle = "Apply delete"
            rejectTitle = "Dismiss"
            note = "Deleting always waits for you. Nothing has changed in the doc yet."
        case (.parked, _):
            acceptTitle = "Apply"
            rejectTitle = "Discard"
            note = "Parked: not applied until you accept it."
        case (.flagged, _):
            acceptTitle = "Keep"
            rejectTitle = "Revert"
            note = nil
        }
    }
}

/// Markdown as the words a reader sees: heading marks, emphasis, code
/// ticks and wikilink brackets dropped, `[text](url)` as its text.
enum ReviewText {
    static func plain(_ markdown: String) -> String {
        var lines: [String] = []
        for raw in markdown.split(separator: "\n", omittingEmptySubsequences: false) {
            var l = String(raw)
            if l.hasPrefix("#") { l = String(l.drop(while: { $0 == "#" })).trimmingCharacters(in: .whitespaces) }
            lines.append(l)
        }
        var s = lines.joined(separator: "\n")
        // [[Target|label]] → label, [[Target]] → Target
        s = s.replacingOccurrences(of: #"\[\[([^\]|]+)\|([^\]]+)\]\]"#, with: "$2", options: .regularExpression)
        s = s.replacingOccurrences(of: #"\[\[([^\]]+)\]\]"#, with: "$1", options: .regularExpression)
        // [text](url) → text
        s = s.replacingOccurrences(of: #"\[([^\]]+)\]\([^)]*\)"#, with: "$1", options: .regularExpression)
        for mark in ["**", "__", "~~", "`"] { s = s.replacingOccurrences(of: mark, with: "") }
        return s.trimmingCharacters(in: .whitespacesAndNewlines)
    }
}

/// A short "when" for a Library row: the time today, "Yest.", the weekday
/// this week, then "19 Oct" (with the year when it isn't this one).
enum ShortWhen {
    static func string(_ date: Date, now: Date = .now, calendar: Calendar = .current) -> String {
        let f = DateFormatter()
        f.calendar = calendar
        f.timeZone = calendar.timeZone
        f.locale = calendar.locale ?? .current
        if calendar.isDate(date, inSameDayAs: now) {
            f.setLocalizedDateFormatFromTemplate("jj:mm")
            return f.string(from: date)
        }
        let days = calendar.dateComponents([.day], from: calendar.startOfDay(for: date), to: calendar.startOfDay(for: now)).day ?? 0
        if days == 1 { return "Yest." }
        if days > 1, days < 7 {
            f.setLocalizedDateFormatFromTemplate("EEE")
            return f.string(from: date)
        }
        f.setLocalizedDateFormatFromTemplate(calendar.isDate(date, equalTo: now, toGranularity: .year) ? "d MMM" : "d MMM yyyy")
        return f.string(from: date)
    }

    /// What VoiceOver says for it.
    static func spoken(_ date: Date, now: Date = .now) -> String {
        "edited \(RelativeTime.string(date, now: now))"
    }
}

/// The doc header's byline: "You and keeper:API · v14 · edited 12:41".
struct DocByline: Equatable {
    /// newest first, at most three distinct names
    var authors: [String]
    var version: Int?
    var edited: Date?

    /// Who wrote the doc, from its op ledger (newest first). The caller is
    /// "You" (the server says which rows are yours, or by name).
    static func make(history: [DocHistoryEntry], me: String?, version: Int?) -> DocByline {
        var names: [String] = []
        for row in history where row.applied {
            let mine = row.principalKind == "human" && (row.principalIsYours == true || (me != nil && row.principalName == me))
            let name = mine ? "You" : (row.principalName.isEmpty ? "someone" : row.principalName)
            if !names.contains(name) { names.append(name) }
            if names.count == 3 { break }
        }
        let edited = history.first { $0.applied && $0.date != nil }?.date
        return DocByline(authors: names, version: version, edited: edited)
    }

    /// "You and keeper:API", "You, Aoife and claude:docs"
    var who: String {
        switch authors.count {
        case 0: return ""
        case 1: return authors[0]
        default: return authors.dropLast().joined(separator: ", ") + " and " + authors.last!
        }
    }
}

/// The outline of a doc: its headings, in order, from the surface's blocks.
struct OutlineEntry: Identifiable, Hashable {
    var id: BlockID
    var level: Int
    var title: String

    static func make(_ blocks: [(id: BlockID, markdown: String)]) -> [OutlineEntry] {
        blocks.compactMap { b in
            let t = b.markdown.trimmingCharacters(in: .whitespaces)
            guard t.hasPrefix("#") else { return nil }
            let level = t.prefix(while: { $0 == "#" }).count
            guard level <= 6 else { return nil }
            let rest = t.dropFirst(level)
            guard rest.first == " " else { return nil }
            let title = ReviewText.plain(String(rest).components(separatedBy: "\n").first ?? "")
            return title.isEmpty ? nil : OutlineEntry(id: b.id, level: level, title: title)
        }
    }
}

/// History rows as the inspector lists them: one person's run of edits in
/// a row (within 15 minutes) is one line, "You · 3 edits".
struct HistoryGroup: Identifiable, Hashable {
    var id: String
    var who: String
    var isAgent: Bool
    var count: Int
    var applied: Bool
    /// "Rewrote a paragraph", "3 edits"
    var summary: String
    var date: Date?

    static func make(_ history: [DocHistoryEntry], gap: TimeInterval = 15 * 60) -> [HistoryGroup] {
        var out: [HistoryGroup] = []
        var rows: [DocHistoryEntry] = []
        func flush() {
            guard let first = rows.first else { return }
            let mine = first.principalIsYours == true && first.principalKind == "human"
            let who = mine ? "You" : (first.principalName.isEmpty ? "someone" : first.principalName)
            let summary = rows.count == 1 ? describe(first) : "\(rows.count) edits"
            out.append(HistoryGroup(id: first.opID, who: who, isAgent: first.principalKind != "human" || Principal.isAgent(first.principalName),
                                    count: rows.count, applied: first.applied, summary: summary, date: first.date))
            rows = []
        }
        for row in history {
            if let last = rows.last {
                let same = last.principalName == row.principalName && last.applied == row.applied
                let near = (last.date ?? .distantPast).timeIntervalSince(row.date ?? .distantPast) < gap
                if !(same && near) { flush() }
            }
            rows.append(row)
        }
        flush()
        return out
    }

    /// One op in words.
    static func describe(_ row: DocHistoryEntry) -> String {
        let subject = DocReviewItem.subject(row.content ?? "")
        switch row.opType {
        case "insert": return "Added \(subject)"
        case "replace": return "Rewrote \(subject)"
        case "delete": return "Deleted a block"
        case "move": return "Moved a block"
        case "rename_doc": return "Renamed the doc"
        case "move_doc": return "Moved the doc"
        case "set_status": return "Changed the status"
        case let t?: return t.replacingOccurrences(of: "_", with: " ").capitalized
        case nil: return "Edited"
        }
    }
}
