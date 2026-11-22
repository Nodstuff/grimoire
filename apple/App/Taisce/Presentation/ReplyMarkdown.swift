import Foundation
import TaisceKit

/// A Claude reply's Markdown, ready to draw: block structure from the doc
/// reader's parser (`BlockRenderer`, swift-markdown) with every run of
/// inline text already an `AttributedString`, so a view's `body` never
/// parses. Built off the main actor (`Sendable`), once per turn.
indirect enum ReplyNode: Hashable, Sendable {
    case heading(level: Int, text: AttributedString)
    case paragraph(AttributedString)
    /// each item is its own blocks (a nested list is a `.list` inside one)
    case list(ordered: Bool, start: Int, items: [ReplyListItem])
    case quote([ReplyNode])
    /// diagrams and HTML blocks too: a chat shows their source, as written
    case code(language: String?, code: String)
    case table(ReplyTable)
    case rule
}

struct ReplyListItem: Hashable, Sendable {
    /// nil = a bullet or number; true/false = a task-list box
    var checked: Bool?
    var children: [ReplyNode]
}

struct ReplyTable: Hashable, Sendable {
    var header: [AttributedString]
    var rows: [[AttributedString]]
    var alignments: [RenderNode.Table.Alignment]
}

enum ReplyMarkdown {
    /// Block Markdown → nodes (headings, lists to any depth, code, quotes,
    /// tables, links and inline styles).
    static func parse(_ markdown: String) -> [ReplyNode] {
        guard !markdown.isEmpty else { return [] }
        return BlockRenderer.render(markdown: markdown).compactMap(node)
    }

    static func node(_ n: RenderNode) -> ReplyNode? {
        switch n {
        case let .heading(level, inline): .heading(level: level, text: InlineMarkdown.attributed(inline))
        case let .paragraph(inline): .paragraph(InlineMarkdown.attributed(inline))
        case let .list(ordered, start, items):
            .list(ordered: ordered, start: start, items: items.map { ReplyListItem(checked: $0.checked, children: $0.children.compactMap(node)) })
        case let .quote(callout, children):
            .quote((callout.map { [.paragraph(AttributedString($0.capitalized))] } ?? []) + children.compactMap(node))
        case let .code(language, code, _): .code(language: language, code: code)
        case let .table(t):
            .table(ReplyTable(header: t.header.map(InlineMarkdown.attributed), rows: t.rows.map { $0.map(InlineMarkdown.attributed) }, alignments: t.alignments))
        case let .diagram(kind, source): .code(language: kind, code: source)
        case let .frontmatter(text): .code(language: nil, code: text)
        case .thematicBreak: .rule
        case let .html(raw): .code(language: "html", code: raw.trimmingCharacters(in: .newlines))
        }
    }

    // MARK: the collapsed preview

    /// The start of a long reply for the collapsed turn: cut at a block
    /// boundary (a blank line outside a code fence) at or before `limit`
    /// characters, so a cut never leaves an open `**`, a half code span or
    /// an unclosed fence. Nil when the whole reply is short enough.
    static func preview(_ reply: String, limit: Int) -> String? {
        guard reply.count > limit else { return nil }
        let limitIndex = reply.index(reply.startIndex, offsetBy: limit)
        let floor = reply.index(reply.startIndex, offsetBy: limit / 3)
        var boundaries: [String.Index] = []   // a block ends here (outside a fence)
        var lineEnds: [String.Index] = []     // a line of prose ends here
        var fence: (marker: Substring, open: String.Index, lastLine: String.Index?)?
        var position = reply.startIndex
        while position < reply.endIndex {
            let lineEnd = reply[position...].firstIndex(of: "\n") ?? reply.endIndex
            let next = lineEnd < reply.endIndex ? reply.index(after: lineEnd) : lineEnd
            if next > limitIndex { break }   // the limit falls on this line
            let line = reply[position..<lineEnd]
            let trimmed = line.drop { $0 == " " }
            if let f = fence {
                if trimmed.hasPrefix(f.marker), trimmed.drop(while: { $0 == f.marker.first }).allSatisfy(\.isWhitespace) {
                    fence = nil
                    boundaries.append(next)
                } else {
                    fence?.lastLine = next
                }
            } else if trimmed.hasPrefix("```") || trimmed.hasPrefix("~~~") {
                fence = (trimmed.prefix { $0 == trimmed.first }, position, nil)
                if position > reply.startIndex { boundaries.append(position) }
            } else if line.allSatisfy(\.isWhitespace) {
                boundaries.append(next)
            } else {
                lineEnds.append(next)
            }
            position = next
        }
        // the last block boundary that keeps a fair share of the limit
        if let b = boundaries.last(where: { $0 >= floor }) {
            return String(reply[..<b]).trimmingCharacters(in: .whitespacesAndNewlines)
        }
        // the limit falls inside a long code block: its whole lines so far, closed again
        if let f = fence, let last = f.lastLine, last >= floor {
            var text = String(reply[..<last])
            if !text.hasSuffix("\n") { text += "\n" }
            return text + f.marker
        }
        // a line break inside a paragraph or between list items
        if fence == nil, let e = lineEnds.last(where: { $0 >= floor }) {
            return balanced(String(reply[..<e]).trimmingCharacters(in: .whitespacesAndNewlines))
        }
        // one long paragraph: at a word, its inline marks balanced, then an ellipsis
        if fence == nil {
            let head = reply[..<limitIndex]
            let cut = head.lastIndex(where: \.isWhitespace).map { String(head[..<$0]) } ?? String(head)
            return balanced(cut) + "\u{2026}"
        }
        // a code block from the start with a line longer than the limit: its first
        // `limit` characters, the fence closed
        let head = String(reply[..<limitIndex])
        return head + "\n" + (fence?.marker ?? "```")
    }

    /// Drops a dangling inline mark at the end of a cut: an odd backtick,
    /// `**` or `__` count, or an unclosed `[link](`.
    static func balanced(_ s: String) -> String {
        var text = s
        for mark in ["`", "**", "__", "~~"] {
            let count = text.components(separatedBy: mark).count - 1
            if count % 2 == 1, let r = text.range(of: mark, options: .backwards) {
                text = String(text[..<r.lowerBound]).trimmingCharacters(in: .whitespaces)
            }
        }
        if let open = text.range(of: "[", options: .backwards), !text[open.lowerBound...].contains(")") {
            text = String(text[..<open.lowerBound]).trimmingCharacters(in: .whitespaces)
        }
        return text
    }
}

/// What a turn's prompt was, as the feed shows it: the person's words, or a
/// line saying what the Mac started the turn with (a background task that
/// finished, mail that woke it), which are not the person's.
struct PromptDisplay: Hashable, Sendable {
    struct Note: Hashable, Sendable {
        var symbol: String
        var text: String
    }

    /// muted system lines, in order (one per background task that finished)
    var notes: [Note] = []
    /// the person's own words, if any (a bubble)
    var words: String

    /// Workbox's wake line (`MailDesk.wakeLine`), typed into the pane when
    /// mail arrives for a session.
    static let wakeLine = "You have mail from another session; call the inbox tool to read it."

    init(prompt: String, source: PromptSource) {
        let trimmed = prompt.trimmingCharacters(in: .whitespacesAndNewlines)
        if source == .wake || trimmed == Self.wakeLine {
            notes = [Note(symbol: "envelope", text: "Woken for mail from another session")]
            words = ""
            return
        }
        guard trimmed.contains("<task-notification") else {
            words = prompt
            return
        }
        var rest = Substring(trimmed)
        var outside = ""
        while let open = rest.range(of: "<task-notification") {
            outside += rest[..<open.lowerBound]
            let block: Substring
            if let close = rest[open.upperBound...].range(of: "</task-notification>") {
                block = rest[open.upperBound..<close.lowerBound]
                rest = rest[close.upperBound...]
            } else {
                // cut short (a prompt kept to fit): the rest is the block
                block = rest[open.upperBound...]
                rest = ""
            }
            notes.append(Note(symbol: "checklist", text: Self.note(block)))
        }
        outside += rest
        words = outside.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    /// "Background task: Agent "Review Apple UI views" finished"
    static func note(_ block: Substring) -> String {
        let summary = tag("summary", in: block).map(unescape)?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        if !summary.isEmpty { return "Background task: \(summary)" }
        if let status = tag("status", in: block).map(unescape)?.trimmingCharacters(in: .whitespacesAndNewlines), !status.isEmpty {
            return "Background task \(status)"
        }
        return "Background task finished"
    }

    /// The text of `<name>…</name>` (to the end when cut short before its close).
    static func tag(_ name: String, in s: Substring) -> Substring? {
        guard let open = s.range(of: "<\(name)>") else { return nil }
        let body = s[open.upperBound...]
        if let close = body.range(of: "</\(name)>") { return body[..<close.lowerBound] }
        // truncated: up to the next tag, or the end
        return body[..<(body.range(of: "<")?.lowerBound ?? body.endIndex)]
    }

    static func unescape<S: StringProtocol>(_ s: S) -> String {
        String(s).replacingOccurrences(of: "&quot;", with: "\"").replacingOccurrences(of: "&apos;", with: "'")
            .replacingOccurrences(of: "&lt;", with: "<").replacingOccurrences(of: "&gt;", with: ">")
            .replacingOccurrences(of: "&amp;", with: "&")
    }
}
