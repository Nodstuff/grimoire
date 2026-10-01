import Foundation
import Markdown

/// A list item's marker: depth, bullet or number, and an optional task box.
public struct ListPrefix: Hashable, Sendable {
    public var indent: Int
    public var ordered: Bool
    /// `-`, `*`, `+` for bullets; `.` or `)` after a number
    public var marker: Character
    /// the number this item was written with (ordered lists renumber from
    /// the first item of each run)
    public var number: Int
    /// nil = not a task; false = `[ ]`; true = `[x]`
    public var checkbox: Bool?

    public init(indent: Int = 0, ordered: Bool = false, marker: Character? = nil, number: Int = 1, checkbox: Bool? = nil) {
        self.indent = indent
        self.ordered = ordered
        self.marker = marker ?? (ordered ? "." : "-")
        self.number = number
        self.checkbox = checkbox
    }

    public static let bullet = ListPrefix()
    public static let task = ListPrefix(checkbox: false)
    public static let numbered = ListPrefix(ordered: true)

    /// The next item's prefix after this one (Return in a list).
    public var continued: ListPrefix {
        var p = self
        if p.ordered { p.number += 1 }
        if p.checkbox != nil { p.checkbox = false }
        return p
    }
}

public struct EditorListItem: Hashable, Sendable {
    public var prefix: ListPrefix
    public var text: AttributedString

    public init(_ prefix: ListPrefix, _ text: AttributedString = AttributedString()) {
        self.prefix = prefix
        self.text = text
    }
}

/// One block as the editor holds it. Text kinds carry inline formatting;
/// `raw` is the block's markdown source, edited as-is (code, diagrams,
/// tables, callouts, and anything the inline model can't hold exactly).
public enum EditorBlockContent: Hashable, Sendable {
    case paragraph(AttributedString)
    case heading(level: Int, AttributedString)
    case quote(AttributedString)
    case list([EditorListItem])
    case raw(String)

    public var isRaw: Bool { if case .raw = self { true } else { false } }
    public var isList: Bool { if case .list = self { true } else { false } }

    /// Every character the block holds, for "is it empty".
    public var isEmpty: Bool {
        switch self {
        case let .paragraph(t), let .heading(_, t), let .quote(t): t.characters.isEmpty
        case let .list(items): items.count == 1 && items[0].text.characters.isEmpty
        case let .raw(s): s.isEmpty
        }
    }

    /// The inline text of the text kinds (a list's items joined by line breaks).
    public var inline: AttributedString {
        switch self {
        case let .paragraph(t), let .heading(_, t), let .quote(t): return t
        case let .list(items):
            var out = AttributedString()
            for (i, item) in items.enumerated() {
                if i > 0 { out += AttributedString("\n") }
                out += item.text
            }
            return out
        case let .raw(s): return AttributedString(s)
        }
    }

    // MARK: parse

    /// The editor's view of a block. Falls back to `.raw(content)` whenever
    /// the structured form would not serialise back to the same markdown
    /// meaning, so editing never changes what it didn't touch.
    public static func parse(_ block: Block) -> EditorBlockContent {
        switch block.blockType {
        case .paragraph, .heading, .decision: return parse(markdown: block.content)
        default: return .raw(block.content)
        }
    }

    public static func parse(markdown: String) -> EditorBlockContent {
        guard let structured = structured(markdown) else { return .raw(markdown) }
        return sameMeaning(markdown, structured.markdown) ? structured : .raw(markdown)
    }

    /// Whether two block sources parse to the same document.
    public static func sameMeaning(_ a: String, _ b: String) -> Bool {
        if a == b { return true }
        let opts: ParseOptions = [.disableSmartOpts]
        return Document(parsing: a, options: opts).format() == Document(parsing: b, options: opts).format()
    }

    static func structured(_ md: String) -> EditorBlockContent? {
        if md.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { return .paragraph(AttributedString()) }
        if md.hasPrefix("---") { return nil } // frontmatter or a rule
        let doc = Document(parsing: md, options: [.disableSmartOpts])
        let children = Array(doc.children)
        guard children.count == 1 else { return nil }
        switch children[0] {
        case is Heading:
            guard md.first == "#", !md.contains("\n") else { return nil } // ATX only
            return headingInline(md).flatMap { level, inline in InlineCodec.parse(inline).map { .heading(level: level, $0) } }
        case is Paragraph:
            return InlineCodec.parse(md).map { .paragraph($0) }
        case let q as BlockQuote:
            let kids = Array(q.children)
            guard kids.count == 1, kids[0] is Paragraph else { return nil }
            let inner = md.split(separator: "\n", omittingEmptySubsequences: false).map { line -> Substring in
                var l = line.drop { $0 == " " }
                if l.first == ">" { l = l.dropFirst(); if l.first == " " { l = l.dropFirst() } }
                return l
            }.joined(separator: "\n")
            if inner.hasPrefix("[!") { return nil } // a callout
            return InlineCodec.parse(inner).map { .quote($0) }
        case is UnorderedList, is OrderedList:
            return list(md, root: children[0])
        default:
            return nil
        }
    }

    /// `## Title ##` → (2, "Title").
    static func headingInline(_ line: String) -> (Int, String)? {
        let level = line.prefix { $0 == "#" }.count
        guard (1...6).contains(level) else { return nil }
        var rest = line.dropFirst(level)
        guard rest.isEmpty || rest.first == " " || rest.first == "\t" else { return nil }
        rest = rest.drop { $0 == " " || $0 == "\t" }
        // an optional closing sequence: spaces then #s at the end
        var trimmed = rest
        while trimmed.last == " " || trimmed.last == "\t" { trimmed = trimmed.dropLast() }
        if trimmed.last == "#" {
            let hashes = trimmed.reversed().prefix { $0 == "#" }.count
            let before = trimmed.dropLast(hashes)
            if before.isEmpty || before.last == " " || before.last == "\t" {
                trimmed = before
                while trimmed.last == " " || trimmed.last == "\t" { trimmed = trimmed.dropLast() }
            }
        }
        return (level, String(trimmed))
    }

    /// A list where every source line is one single-paragraph item.
    static func list(_ md: String, root: any Markup) -> EditorBlockContent? {
        var depths: [(Int, Bool?)] = []
        func walk(_ m: any Markup, depth: Int) -> Bool {
            for li in m.children {
                guard let item = li as? Markdown.ListItem else { return false }
                var sawParagraph = false
                for c in item.children {
                    if c is Paragraph, !sawParagraph {
                        sawParagraph = true
                    } else if c is UnorderedList || c is OrderedList {
                        continue
                    } else {
                        return false
                    }
                }
                depths.append((depth, item.checkbox.map { $0 == .checked }))
                for c in item.children where c is UnorderedList || c is OrderedList {
                    guard walk(c, depth: depth + 1) else { return false }
                }
            }
            return true
        }
        guard walk(root, depth: 0) else { return nil }
        let lines = md.split(separator: "\n", omittingEmptySubsequences: false)
        guard lines.count == depths.count else { return nil }
        var items: [EditorListItem] = []
        for (line, (depth, box)) in zip(lines, depths) {
            guard let (prefix, rest) = itemPrefix(String(line), depth: depth), prefix.checkbox == box,
                  let text = InlineCodec.parse(rest)
            else { return nil }
            items.append(EditorListItem(prefix, text))
        }
        // cmark numbers from each list's first item (`1. a\n1. b` shows 1, 2):
        // so does the model, and a touched list is written out that way
        return .list(EditorCommands.renumbered(items))
    }

    /// `  - [ ] text` → (prefix, "text").
    static func itemPrefix(_ line: String, depth: Int) -> (ListPrefix, String)? {
        var s = Substring(line).drop { $0 == " " || $0 == "\t" }
        var prefix = ListPrefix(indent: depth)
        if let c = s.first, "-*+".contains(c) {
            prefix.marker = c
            s = s.dropFirst()
        } else {
            let digits = s.prefix { $0.isASCII && $0.isNumber }
            guard (1...9).contains(digits.count), let n = Int(digits) else { return nil }
            s = s.dropFirst(digits.count)
            guard let d = s.first, d == "." || d == ")" else { return nil }
            prefix.ordered = true
            prefix.marker = d
            prefix.number = n
            s = s.dropFirst()
        }
        guard s.isEmpty || s.first == " " || s.first == "\t" else { return nil }
        s = s.drop { $0 == " " || $0 == "\t" }
        for (box, checked) in [("[ ]", false), ("[x]", true), ("[X]", true)] where s.hasPrefix(box) {
            let after = s.dropFirst(3)
            if after.isEmpty || after.first == " " {
                prefix.checkbox = checked
                s = after.drop { $0 == " " }
            }
            break
        }
        return (prefix, String(s))
    }

    // MARK: serialise

    /// Canonical markdown for the block.
    public var markdown: String {
        switch self {
        case let .paragraph(t):
            return InlineCodec.serialize(t)
        case let .heading(level, t):
            var inline = InlineCodec.serialize(Self.singleLine(t))
            // a trailing ` #` run would read as the closing sequence: escape it
            if inline.hasSuffix("#") {
                let run = inline.reversed().prefix { $0 == "#" }.count
                let before = inline.dropLast(run)
                if before.isEmpty || before.last == " " || before.last == "\t" {
                    inline = String(before) + "\\" + String(repeating: "#", count: run)
                }
            }
            return String(repeating: "#", count: max(1, min(6, level))) + " " + inline
        case let .quote(t):
            let inline = InlineCodec.serialize(t)
            return inline.split(separator: "\n", omittingEmptySubsequences: false).map { "> " + $0 }.joined(separator: "\n")
        case let .list(items):
            return Self.serialize(items)
        case let .raw(s):
            return s
        }
    }

    static func singleLine(_ t: AttributedString) -> AttributedString {
        var t = t
        while let r = t.characters.firstIndex(of: "\n") {
            t.replaceSubrange(r..<t.characters.index(after: r), with: AttributedString(" "))
        }
        return t
    }

    static func serialize(_ items: [EditorListItem]) -> String {
        var lines: [String] = []
        // content column and running number per depth
        var columns: [Int] = []
        var numbers: [Int?] = []
        var prevDepth = -1
        for item in items {
            let depth = max(0, min(item.prefix.indent, prevDepth + 1))
            if depth < columns.count {
                // back out of a sublist: this level's run carries on
                columns.removeSubrange((depth + 1)...)
                numbers.removeSubrange((depth + 1)...)
            }
            let lead = depth == 0 ? 0 : columns[depth - 1]
            var marker: String
            if item.prefix.ordered {
                let n: Int
                if depth < numbers.count, let last = numbers[depth] {
                    n = last + 1
                } else {
                    n = item.prefix.number
                }
                if depth < numbers.count { numbers[depth] = n } else { numbers.append(n) }
                marker = "\(n)\(item.prefix.marker)"
            } else {
                if depth < numbers.count { numbers[depth] = nil } else { numbers.append(nil) }
                marker = String(item.prefix.marker)
            }
            let col = lead + marker.count + 1
            if depth < columns.count { columns[depth] = col } else { columns.append(col) }
            var line = String(repeating: " ", count: lead) + marker + " "
            if let box = item.prefix.checkbox { line += box ? "[x] " : "[ ] " }
            line += InlineCodec.serialize(singleLine(item.text))
            lines.append(line)
            prevDepth = depth
        }
        return lines.joined(separator: "\n")
    }
}
