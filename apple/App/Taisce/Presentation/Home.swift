import Foundation
import TaisceKit

/// Today's "Continue reading": the newest book in progress, from the
/// Books shelf (`BookStore`), so Today and Books never disagree.
enum ContinueReading {
    /// The first book that is started and not finished (the list is newest first).
    static func pick(_ shelf: BookShelf) -> ReadingBook? {
        shelf.continueReading
            .first { $0.progress.percentage > 0 && $0.progress.percentage < 100 }
            .map { ReadingBook(book: $0.book, progress: $0.progress) }
    }

    /// "64%", rounded down so 99.6 never reads as done.
    static func percent(_ p: BookProgress) -> String {
        "\(Int(p.percentage.rounded(.down)))%"
    }

    /// "Aaron Dembski-Bowden · Book 14"
    static func byline(_ book: Book) -> String {
        let n = book.seriesIndex.map { "Book \(BookFormat.number($0))" }
        return [book.authorName, n].compactMap { $0 }.joined(separator: " · ")
    }

    /// "Kindle · 2 h ago" (either half omitted when unknown)
    static func whereAndWhen(_ p: BookProgress, now: Date = .now) -> String? {
        let when = p.updatedAt.map { RelativeTime.string($0, now: now) }
        let parts = [p.device.isEmpty ? nil : p.device, when].compactMap { $0 }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }
}

/// The words on Today's Review card and the review list.
enum ReviewCopy {
    /// "3 agent changes want a look" / "1 change wants a look"
    static func title(_ items: [ReviewQueueItem]) -> String {
        let n = items.count
        let agents = !items.isEmpty && items.allSatisfy(\.byAgent)
        let noun = (agents ? "agent change" : "change") + (n == 1 ? "" : "s")
        return "\(n) \(noun) \(n == 1 ? "wants" : "want") a look"
    }

    /// What deciding means for what's waiting.
    static func subtitle(_ summary: ReviewQueueSummary) -> String {
        switch (summary.flagged, summary.parked) {
        case (_, 0): "Already in your docs. Keep or revert."
        case (0, _): "Not applied yet. Apply or discard."
        case let (f, p): "\(f) already in your docs, \(p) waiting to apply."
        }
    }

    static func verdict(_ kind: ReviewQueueItem.Kind) -> Verdict {
        kind == .flagged ? .yellow : .red
    }

    /// The two decisions as the buttons name them.
    static func actions(_ kind: ReviewQueueItem.Kind) -> (accept: String, decline: String) {
        kind == .flagged ? ("Keep", "Revert") : ("Apply", "Discard")
    }

    /// A parked doc delete applies a delete: the button says so and asks first.
    static func acceptIsDestructive(_ item: ReviewQueueItem) -> Bool {
        item.kind == .parked && (item.summary.hasPrefix("Move \u{201C}") || item.summary.hasPrefix("Delete"))
    }

    static func failure(_ error: Error) -> String {
        switch error {
        case let APIError.server(msg): msg
        case APIError.notFound: "That change was already decided."
        case let e as APIError where e.isForbidden: "You can't decide changes in this workspace."
        default: error.localizedDescription
        }
    }
}

/// Today's Due list: overdue, today's dated items, then the week ahead.
enum TodayDue {
    static let limit = 5
    static let horizonDays = 7

    static func rows(_ board: TodoBoard, now: Date = .now, calendar: Calendar = .current) -> [TodoEntry] {
        let today = Due.today(now: now, in: calendar.timeZone).dateString
        let soon = board.upcoming.filter { e in
            guard let due = e.due else { return false }
            return DueLabel.dayDistance(from: today, to: due.dateString, calendar: calendar) <= horizonDays
        }
        return board.overdue + board.today.filter { $0.due != nil } + soon
    }
}

/// What Search shows of an answer doc (crates/daemon/src/ask.rs): the
/// synthesis paragraph(s) under the question, and the docs its receipts
/// come from (each a `## Title` section of quoted excerpts).
struct AskAnswerText: Equatable, Sendable {
    /// the synthesis, links read as their titles; nil when there is none
    var body: String?
    /// cited docs, in the order the answer lists them
    var sources: [String]
    /// the synthesis placeholder is still there: the model is writing
    var pending: Bool

    static let placeholderMark = "Writing a synthesis"

    static func parse(_ markdown: String) -> AskAnswerText {
        var lines = markdown.components(separatedBy: "\n")
        // frontmatter
        if lines.first?.trimmingCharacters(in: .whitespaces) == "---",
           let end = lines.dropFirst().firstIndex(where: { $0.trimmingCharacters(in: .whitespaces) == "---" }) {
            lines.removeSubrange(0...end)
        }
        var paragraphs: [String] = []
        var current: [String] = []
        var sources: [String] = []
        var pending = false
        var inSources = false
        func flush() {
            let p = current.joined(separator: " ").trimmingCharacters(in: .whitespaces)
            if !p.isEmpty { paragraphs.append(p) }
            current = []
        }
        for raw in lines {
            let line = raw.trimmingCharacters(in: .whitespaces)
            if line.hasPrefix("## ") {
                flush()
                inSources = true
                let title = String(line.dropFirst(3)).trimmingCharacters(in: .whitespaces)
                if !title.isEmpty, !sources.contains(title) { sources.append(title) }
                continue
            }
            if line == "---" { flush(); break } // the "Asked … · N blocks" footer
            guard !inSources else { continue }
            if line.hasPrefix("# ") || line.hasPrefix(">") { flush(); continue }
            if line.isEmpty { flush(); continue }
            if line.contains(placeholderMark) { pending = true; continue }
            current.append(line)
        }
        flush()
        let body = paragraphs.isEmpty ? nil : paragraphs.map(readLinks).joined(separator: "\n\n")
        return AskAnswerText(body: body, sources: sources, pending: pending)
    }

    /// `[[Title#^id|alias]]` → alias, `[[Title#^id]]` → Title.
    static func readLinks(_ s: String) -> String {
        var out = ""
        var rest = Substring(s)
        while let open = rest.range(of: "[[") {
            out += rest[..<open.lowerBound]
            guard let close = rest[open.upperBound...].range(of: "]]") else {
                out += rest[open.lowerBound...]
                return out
            }
            let inner = rest[open.upperBound..<close.lowerBound]
            if let bar = inner.firstIndex(of: "|") {
                out += inner[inner.index(after: bar)...]
            } else {
                out += inner.split(separator: "#", maxSplits: 1).first.map(String.init) ?? ""
            }
            rest = rest[close.upperBound...]
        }
        return out + rest
    }
}

/// The To-dos header line: "6 open · 1 overdue".
enum TodosSummary {
    static func line(_ board: TodoBoard) -> String {
        var parts = ["\(board.count) open"]
        if !board.overdue.isEmpty { parts.append("\(board.overdue.count) overdue") }
        if !board.today.isEmpty { parts.append("\(board.today.count) today") }
        return parts.joined(separator: " · ")
    }
}
