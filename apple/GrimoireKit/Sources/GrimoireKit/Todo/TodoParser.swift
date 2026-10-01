import Foundation

/// Reads the To-do doc's markdown offline. Mirrors the canonical form the
/// daemon writes (crates/daemon/src/todo.rs):
///
///     ## 2026-09-10
///     - [ ] Ship 0.8.0 · due 2026-09-12 14:30 (carried from 2026-09-09)
///       an indented note
///
/// Only the canonical ` · due D[ HH:MM]` and legacy `⏰ D` deadlines are read;
/// natural phrases ("due fri") are the server's job and stay in the text.
public enum TodoParser {
    public static let todoDocTitle = "To-do"
    static let stamp = "(carried from "

    public static func parse(markdown: String) -> [TodoRecord] {
        var out: [TodoRecord] = []
        var day: String?
        var position = 0
        var current: TodoRecord?
        var noteLines: [String] = []

        func flush() {
            guard var c = current else { return }
            let note = noteLines.joined(separator: "\n").trimmingCharacters(in: .whitespacesAndNewlines)
            c.note = note.isEmpty ? nil : note
            out.append(c)
            current = nil
            noteLines = []
        }

        for raw in markdown.split(separator: "\n", omittingEmptySubsequences: false) {
            let line = String(raw)
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            if let d = dayHeading(trimmed) {
                flush()
                day = d
                position = 0
                continue
            }
            if trimmed.hasPrefix("#") {
                flush()
                // `### sub` stays inside the day; `# x` / `## other` ends it
                if !trimmed.hasPrefix("###") { day = nil }
                continue
            }
            guard let day else { continue }
            if let (mark, rest) = splitItemLine(trimmed) {
                flush()
                let t = tokens(rest)
                current = TodoRecord(
                    date: day, position: position, mark: String(mark), text: t.text,
                    deadline: t.deadline, carriedFrom: t.carriedFrom, note: nil
                )
                position += 1
            } else if trimmed.isEmpty {
                flush()
            } else if current != nil {
                noteLines.append(deindent(line))
            }
        }
        flush()
        return out
    }

    /// `## 2026-09-10` → "2026-09-10"
    static func dayHeading(_ line: String) -> String? {
        guard line.hasPrefix("## ") else { return nil }
        let d = line.dropFirst(3).trimmingCharacters(in: .whitespaces)
        return Due(d).flatMap { $0.hasTime ? nil : d }
    }

    /// `- [x] rest` → ("x", "rest"); `*` bullets and `X` tolerated.
    static func splitItemLine(_ line: String) -> (Character, String)? {
        guard line.hasPrefix("- ") || line.hasPrefix("* ") else { return nil }
        let rest = line.dropFirst(2).drop(while: { $0 == " " })
        guard rest.count >= 3, rest.first == "[" else { return nil }
        let markIdx = rest.index(after: rest.startIndex)
        let closeIdx = rest.index(after: markIdx)
        guard rest[closeIdx] == "]" else { return nil }
        var mark = rest[markIdx]
        guard [" ", "x", "X", ">"].contains(mark) else { return nil }
        if mark == "X" { mark = "x" }
        return (mark, rest[rest.index(after: closeIdx)...].trimmingCharacters(in: .whitespaces))
    }

    struct Tokens: Equatable {
        var text: String
        var deadline: String?
        var carriedFrom: String?
    }

    static func tokens(_ raw: String) -> Tokens {
        var t = raw.trimmingCharacters(in: .whitespaces)
        var carriedFrom: String?
        if t.hasSuffix(")"), let open = t.range(of: stamp, options: .backwards) {
            let d = String(t[open.upperBound..<t.index(before: t.endIndex)])
            if Due(d) != nil {
                carriedFrom = d
                t = String(t[..<open.lowerBound]).trimmingCharacters(in: .whitespaces)
            }
        }
        var deadline: String?
        // canonical ` · due D[ HH:MM]` — the LAST separator, so a `·` in the text survives
        if let sep = t.range(of: " · due ", options: .backwards) {
            let cand = String(t[sep.upperBound...]).trimmingCharacters(in: .whitespaces)
            if let due = Due(cand) {
                deadline = due.description
                t = String(t[..<sep.lowerBound])
            }
        } else if let clock = t.range(of: "⏰") {
            let after = t[clock.upperBound...].trimmingCharacters(in: .whitespaces)
            let cand = String(after.prefix(10))
            if let due = Due(cand) {
                deadline = due.description
                let tail = after.dropFirst(10).trimmingCharacters(in: .whitespaces)
                t = [t[..<clock.lowerBound].trimmingCharacters(in: .whitespaces), tail]
                    .filter { !$0.isEmpty }.joined(separator: " ")
            }
        }
        let text = t.split(whereSeparator: \.isWhitespace).joined(separator: " ")
        return Tokens(text: text, deadline: deadline, carriedFrom: carriedFrom)
    }

    static func deindent(_ line: String) -> String {
        if line.hasPrefix("  ") { return String(line.dropFirst(2)) }
        if line.hasPrefix("\t") { return String(line.dropFirst()) }
        return line.trimmingCharacters(in: .whitespaces)
    }

    /// Open items due on or before `now`'s day, or overdue by time — the
    /// Today view's "due / overdue" list, soonest first.
    public static func dueOrOverdue(_ items: [TodoRecord], now: Date = .now, timeZone: TimeZone = .current) -> [TodoRecord] {
        var cal = Calendar(identifier: .gregorian)
        cal.timeZone = timeZone
        let c = cal.dateComponents([.year, .month, .day], from: now)
        guard let y = c.year, let m = c.month, let d = c.day else { return [] }
        let endOfToday = Due(year: y, month: m, day: d, hour: 23, minute: 59)
        return items
            .filter { $0.isOpen && ($0.due.map { $0 <= endOfToday } ?? false) }
            .sorted { ($0.due ?? endOfToday) < ($1.due ?? endOfToday) }
    }
}
