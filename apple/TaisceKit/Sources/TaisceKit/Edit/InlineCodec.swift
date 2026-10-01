import Foundation
import Markdown

/// Inline formatting the editor models. Everything else inline (images,
/// HTML, hard breaks, titled links) keeps its block in raw source mode.
public struct InlineMarks: OptionSet, Hashable, Sendable, Codable {
    public let rawValue: Int
    public init(rawValue: Int) { self.rawValue = rawValue }

    public static let bold = InlineMarks(rawValue: 1 << 0)
    public static let italic = InlineMarks(rawValue: 1 << 1)
    public static let code = InlineMarks(rawValue: 1 << 2)
    public static let strike = InlineMarks(rawValue: 1 << 3)
}

public enum InlineMarksKey: AttributedStringKey {
    public typealias Value = InlineMarks
    public static let name = "taisce.marks"
}

/// A `[text](destination)` link's destination.
public enum LinkTargetKey: AttributedStringKey {
    public typealias Value = String
    public static let name = "taisce.link"
}

/// A `[[...]]` wikilink: the value is everything between the brackets
/// (`Doc`, `Doc|alias`, `Doc#^id`), the text is what it shows. Wikilinks
/// are atomic in the editor, so the text never drifts from the value.
public enum WikiLinkKey: AttributedStringKey {
    public typealias Value = String
    public static let name = "taisce.wiki"
}

public extension AttributeScopes {
    struct TaisceAttributes: AttributeScope {
        public let taisceMarks: InlineMarksKey
        public let taisceLink: LinkTargetKey
        public let taisceWiki: WikiLinkKey
    }

    var taisce: TaisceAttributes.Type { TaisceAttributes.self }
}

public extension AttributeDynamicLookup {
    subscript<T: AttributedStringKey>(dynamicMember keyPath: KeyPath<AttributeScopes.TaisceAttributes, T>) -> T {
        self[T.self]
    }
}

/// One stretch of inline text with uniform formatting.
public struct InlineRun: Hashable, Sendable {
    public var text: String
    public var marks: InlineMarks
    public var link: String?
    public var wiki: String?

    public init(_ text: String, marks: InlineMarks = [], link: String? = nil, wiki: String? = nil) {
        self.text = text
        self.marks = marks
        self.link = link
        self.wiki = wiki
    }
}

/// Inline markdown ↔ `AttributedString` (the editor's model): bold,
/// italic, inline code, strikethrough, links and `[[wikilinks]]`.
///
/// `parse` returns nil for anything it can't represent exactly, and the
/// block codec then edits that block as raw source instead. `serialize`
/// is canonical (`**bold**`, `*italic*`, `~~strike~~`) and escapes only
/// what would otherwise re-parse differently, checking itself by parsing
/// its own output.
public enum InlineCodec {
    // MARK: runs

    public static func runs(_ text: AttributedString) -> [InlineRun] {
        text.runs.map { run in
            InlineRun(
                String(text[run.range].characters),
                marks: run.taisceMarks ?? [],
                link: run.taisceLink,
                wiki: run.taisceWiki
            )
        }
    }

    public static func build(_ runs: [InlineRun]) -> AttributedString {
        var out = AttributedString()
        for r in runs where !r.text.isEmpty {
            var piece = AttributedString(r.text)
            if !r.marks.isEmpty { piece.taisceMarks = r.marks }
            if let l = r.link { piece.taisceLink = l }
            if let w = r.wiki { piece.taisceWiki = w }
            out += piece
        }
        return out
    }

    /// What a wikilink shows: the alias after `|`, else the target.
    public static func wikiDisplay(_ inner: String) -> String {
        if let bar = inner.firstIndex(of: "|") { return String(inner[inner.index(after: bar)...]) }
        return inner
    }

    public static func wiki(_ inner: String, marks: InlineMarks = []) -> AttributedString {
        build([InlineRun(wikiDisplay(inner), marks: marks, wiki: inner)])
    }

    // MARK: parse

    static let placeholderOpen: Character = "\u{F8F0}"
    static let placeholderClose: Character = "\u{F8F1}"

    /// Inline markdown (soft line breaks allowed) → attributed text, or nil
    /// when it holds something the editor doesn't model.
    public static func parse(_ markdown: String) -> AttributedString? {
        if markdown.contains(placeholderOpen) || markdown.contains(placeholderClose) { return nil }
        let (protected, wikis) = protectWikiLinks(markdown)
        let doc = Document(parsing: protected, options: [.disableSmartOpts])
        let children = Array(doc.children)
        if children.isEmpty { return AttributedString() }
        guard children.count == 1, let p = children[0] as? Paragraph else { return nil }
        var runs: [InlineRun] = []
        for child in p.children {
            guard walk(child, marks: [], link: nil, wikis: wikis, into: &runs) else { return nil }
        }
        return normalized(build(runs))
    }

    /// `[[inner]]` → a private-use placeholder, so cmark never reads the
    /// brackets (or emphasis inside a title) as markdown.
    static func protectWikiLinks(_ s: String) -> (String, [String]) {
        var out = ""
        var wikis: [String] = []
        var i = s.startIndex
        while i < s.endIndex {
            if s[i] == "\\", s.index(after: i) < s.endIndex {
                // an escaped character stays escaped (`\[\[` is not a link)
                out.append(s[i])
                out.append(s[s.index(after: i)])
                i = s.index(i, offsetBy: 2)
                continue
            }
            if s[i...].hasPrefix("[["), let close = wikiClose(s, from: s.index(i, offsetBy: 2)) {
                let inner = String(s[s.index(i, offsetBy: 2)..<close])
                out.append(placeholderOpen)
                out += String(wikis.count)
                out.append(placeholderClose)
                wikis.append(inner)
                i = s.index(close, offsetBy: 2)
                continue
            }
            out.append(s[i])
            i = s.index(after: i)
        }
        return (out, wikis)
    }

    /// The `]]` closing a wikilink whose inner text starts at `from`: no
    /// brackets or newlines inside, not empty.
    static func wikiClose(_ s: String, from: String.Index) -> String.Index? {
        var j = from
        while j < s.endIndex {
            let c = s[j]
            if c == "]" {
                guard j > from, s[j...].hasPrefix("]]") else { return nil }
                return j
            }
            if c == "[" || c == "\n" { return nil }
            j = s.index(after: j)
        }
        return nil
    }

    /// Text with placeholders back to runs: wikilinks, or (inside code)
    /// their literal source.
    static func restore(_ s: String, marks: InlineMarks, link: String?, wikis: [String], literal: Bool) -> [InlineRun]? {
        var out: [InlineRun] = []
        var buf = ""
        var i = s.startIndex
        while i < s.endIndex {
            let c = s[i]
            if c == placeholderOpen {
                guard let close = s[i...].firstIndex(of: placeholderClose),
                      let n = Int(s[s.index(after: i)..<close]), n < wikis.count
                else { return nil }
                if literal {
                    buf += "[[\(wikis[n])]]"
                } else {
                    // a wikilink inside a [link](...) has no markdown meaning we keep
                    if link != nil { return nil }
                    if !buf.isEmpty { out.append(InlineRun(buf, marks: marks, link: link)); buf = "" }
                    out.append(InlineRun(wikiDisplay(wikis[n]), marks: marks, wiki: wikis[n]))
                }
                i = s.index(after: close)
                continue
            }
            if c == placeholderClose { return nil }
            buf.append(c)
            i = s.index(after: i)
        }
        if !buf.isEmpty { out.append(InlineRun(buf, marks: marks, link: link)) }
        return out
    }

    static func walk(_ m: any Markup, marks: InlineMarks, link: String?, wikis: [String], into runs: inout [InlineRun]) -> Bool {
        switch m {
        case let t as Markdown.Text:
            guard let r = restore(t.string, marks: marks, link: link, wikis: wikis, literal: false) else { return false }
            runs += r
            return true
        case is SoftBreak:
            runs.append(InlineRun("\n", marks: marks, link: link))
            return true
        case let c as InlineCode:
            guard !c.code.isEmpty, let r = restore(c.code, marks: marks.union(.code), link: link, wikis: wikis, literal: true) else { return false }
            runs += r
            return true
        case let e as Emphasis:
            return walkChildren(e, marks: marks.union(.italic), link: link, wikis: wikis, into: &runs)
        case let s as Strong:
            return walkChildren(s, marks: marks.union(.bold), link: link, wikis: wikis, into: &runs)
        case let s as Strikethrough:
            return walkChildren(s, marks: marks.union(.strike), link: link, wikis: wikis, into: &runs)
        case let l as Markdown.Link:
            guard link == nil, l.title == nil else { return false }
            let dest = l.destination ?? ""
            if dest.contains(placeholderOpen) { return false }
            var inner: [InlineRun] = []
            guard walkChildren(l, marks: marks, link: dest, wikis: wikis, into: &inner), !inner.isEmpty else { return false }
            runs += inner
            return true
        default:
            // images, inline HTML, hard breaks, symbol links: not modelled
            return false
        }
    }

    static func walkChildren(_ m: any Markup, marks: InlineMarks, link: String?, wikis: [String], into runs: inout [InlineRun]) -> Bool {
        for c in m.children {
            guard walk(c, marks: marks, link: link, wikis: wikis, into: &runs) else { return false }
        }
        return true
    }

    // MARK: normalise

    /// The form both sides of a round trip agree on: no emphasis on the
    /// whitespace at a run's edge (markdown can't put a delimiter there),
    /// no spaces around line breaks or at the ends (cmark drops them), and
    /// no formatting on line breaks at a mark's edge.
    public static func normalized(_ text: AttributedString) -> AttributedString {
        var runs = runs(text)
        // characters with their attributes, flattened for whitespace surgery
        var chars: [(Character, InlineMarks, String?, String?)] = runs.flatMap { r in r.text.map { ($0, r.marks, r.link, r.wiki) } }
        // trim spaces at line edges (never inside wikilinks)
        func isSpace(_ c: Character) -> Bool { c == " " || c == "\t" }
        var kept: [(Character, InlineMarks, String?, String?)] = []
        var i = 0
        while i < chars.count {
            let (c, _, _, w) = chars[i]
            if isSpace(c), w == nil {
                // a run of spaces: drop it at the start, the end, or next to a newline
                var j = i
                while j < chars.count, isSpace(chars[j].0), chars[j].3 == nil { j += 1 }
                let atStart = kept.isEmpty || kept.last?.0 == "\n"
                let atEnd = j == chars.count || chars[j].0 == "\n"
                if !(atStart || atEnd) { kept += chars[i..<j] }
                i = j
                continue
            }
            kept.append(chars[i])
            i += 1
        }
        chars = kept
        // emphasis marks (not code) off the whitespace at each mark's edges
        for mark in [InlineMarks.bold, .italic, .strike] {
            var k = 0
            while k < chars.count {
                guard chars[k].1.contains(mark) else { k += 1; continue }
                var end = k
                while end < chars.count, chars[end].1.contains(mark) { end += 1 }
                var lo = k, hi = end
                while lo < hi, chars[lo].0.isWhitespace, chars[lo].3 == nil, !chars[lo].1.contains(.code) { lo += 1 }
                while hi > lo, chars[hi - 1].0.isWhitespace, chars[hi - 1].3 == nil, !chars[hi - 1].1.contains(.code) { hi -= 1 }
                for x in k..<lo { chars[x].1.remove(mark) }
                for x in hi..<end { chars[x].1.remove(mark) }
                k = end
            }
        }
        // links don't start or end on whitespace either (the text keeps it)
        runs = []
        for (c, m, l, w) in chars {
            if let last = runs.last, last.marks == m, last.link == l, last.wiki == w {
                runs[runs.count - 1].text.append(c)
            } else {
                runs.append(InlineRun(String(c), marks: m, link: l, wiki: w))
            }
        }
        return build(runs)
    }

    // MARK: serialize

    public static func serialize(_ text: AttributedString) -> String {
        let model = normalized(text)
        let minimal = emit(model, escapeAll: false)
        if parse(minimal) == model { return minimal }
        let full = emit(model, escapeAll: true)
        if parse(full) == model || model.characters.isEmpty { return full }
        // some formatting has no markdown spelling here (a delimiter between
        // punctuation and a letter); the text itself survives either way
        return full
    }

    /// One element of the emitted sequence: plain or code text, a wikilink,
    /// or a link group.
    enum Element {
        case text(String, InlineMarks)
        case code(String, InlineMarks)
        case wiki(String, InlineMarks)
        case link(String, [InlineRun], InlineMarks)

        var marks: InlineMarks {
            switch self {
            case let .text(_, m), let .code(_, m), let .wiki(_, m), let .link(_, _, m): m
            }
        }
    }

    static let emphasis: [InlineMarks] = [.bold, .italic, .strike]

    static func delimiter(_ m: InlineMarks) -> String {
        switch m {
        case .bold: "**"
        case .italic: "*"
        default: "~~"
        }
    }

    static func elements(_ runs: [InlineRun]) -> [Element] {
        var out: [Element] = []
        var i = 0
        while i < runs.count {
            let r = runs[i]
            if let w = r.wiki {
                // adjacent identical wikilinks merge into one run: split them back
                let display = wikiDisplay(w)
                var rest = Substring(r.text)
                while !display.isEmpty, rest.hasPrefix(display) {
                    out.append(.wiki(w, r.marks.subtracting(.code)))
                    rest = rest.dropFirst(display.count)
                }
                if !rest.isEmpty { out.append(.text(String(rest), r.marks.subtracting(.code))) }
                i += 1
            } else if let l = r.link {
                var group: [InlineRun] = []
                while i < runs.count, runs[i].link == l, runs[i].wiki == nil {
                    group.append(runs[i])
                    i += 1
                }
                let shared = group.dropFirst().reduce(group[0].marks) { $0.intersection($1.marks) }.subtracting(.code)
                out.append(.link(l, group.map { var g = $0; g.marks.subtract(shared); g.link = nil; return g }, shared))
            } else if r.marks.contains(.code) {
                out.append(.code(r.text, r.marks.subtracting(.code)))
                i += 1
            } else {
                out.append(.text(r.text, r.marks))
                i += 1
            }
        }
        return out
    }

    static func emit(_ text: AttributedString, escapeAll: Bool) -> String {
        let all = runs(text)
        let plain = all.map(\.text).joined()
        return emit(runs: all, escapeAll: escapeAll, context: Array(plain), base: 0, inLabel: false)
    }

    /// The runs as markdown. `context` is the whole paragraph's text, for
    /// the escapes that depend on neighbours and line starts.
    static func emit(runs: [InlineRun], escapeAll: Bool, context: [Character], base: Int, inLabel: Bool) -> String {
        let els = elements(runs)
        var out = ""
        var stack: [InlineMarks] = []
        var pos = base // index into `context` of the next character
        for (idx, el) in els.enumerated() {
            let want = el.marks.intersection([.bold, .italic, .strike])
            // close down to marks that continue
            while let top = stack.last, !stack.allSatisfy({ want.contains($0) }) {
                out += delimiter(top)
                stack.removeLast()
            }
            // open the rest, longest-lasting first, so nesting closes cleanly
            let opening = emphasis.filter { want.contains($0) && !stack.contains($0) }
            let ordered = opening.sorted { extent($0, from: idx, in: els) > extent($1, from: idx, in: els) }
            for m in ordered {
                out += delimiter(m)
                stack.append(m)
            }
            switch el {
            case let .text(s, _):
                out += escape(s, at: pos, in: context, all: escapeAll, inLabel: inLabel)
                pos += s.count
            case let .code(s, _):
                out += codeSpan(s)
                pos += s.count
            case let .wiki(w, _):
                out += "[[\(w)]]"
                pos += wikiDisplay(w).count
            case let .link(dest, inner, _):
                let label = inner.map(\.text).joined()
                if inner.allSatisfy({ $0.marks.isEmpty }), label == dest, isAutolinkable(dest) {
                    out += "<\(dest)>"
                } else {
                    out += "[" + emit(runs: inner, escapeAll: escapeAll, context: context, base: pos, inLabel: true) + "](" + destination(dest) + ")"
                }
                pos += label.count
            }
        }
        while let top = stack.popLast() { out += delimiter(top) }
        return out
    }

    /// How many consecutive elements from `i` carry `m`.
    static func extent(_ m: InlineMarks, from i: Int, in els: [Element]) -> Int {
        var n = 0
        var j = i
        while j < els.count, els[j].marks.contains(m) { n += 1; j += 1 }
        return n
    }

    static func codeSpan(_ s: String) -> String {
        let code = s.replacingOccurrences(of: "\n", with: " ")
        var longest = 0, run = 0
        for c in code {
            if c == "`" { run += 1; longest = max(longest, run) } else { run = 0 }
        }
        let fence = String(repeating: "`", count: longest + 1)
        let pad = code.hasPrefix("`") || code.hasSuffix("`")
            || (code.hasPrefix(" ") && code.hasSuffix(" ") && !code.allSatisfy { $0 == " " })
        return pad ? "\(fence) \(code) \(fence)" : "\(fence)\(code)\(fence)"
    }

    static func isAutolinkable(_ dest: String) -> Bool {
        guard !dest.isEmpty, !dest.contains(where: { $0.isWhitespace || $0 == "<" || $0 == ">" }) else { return false }
        if let colon = dest.firstIndex(of: ":") {
            let scheme = dest[..<colon]
            return (2...32).contains(scheme.count) && scheme.first!.isLetter
                && scheme.allSatisfy { $0.isASCII && ($0.isLetter || $0.isNumber || "+.-".contains($0)) }
        }
        return false
    }

    static func destination(_ d: String) -> String {
        var depth = 0
        var balanced = true
        for c in d {
            if c == "(" { depth += 1 }
            if c == ")" { depth -= 1; if depth < 0 { balanced = false } }
        }
        if depth != 0 { balanced = false }
        let escaped = d.replacingOccurrences(of: "\\", with: "\\\\")
        if !balanced || d.contains(where: { $0.isWhitespace || $0 == "<" || $0 == ">" }) {
            return "<" + escaped.replacingOccurrences(of: "<", with: "\\<").replacingOccurrences(of: ">", with: "\\>") + ">"
        }
        return escaped
    }

    static func isASCIIPunct(_ c: Character) -> Bool {
        guard let a = c.asciiValue else { return false }
        return (33...47).contains(a) || (58...64).contains(a) || (91...96).contains(a) || (123...126).contains(a)
    }

    /// Escapes for plain text at `start` in `ctx`. Minimal by default:
    /// only what would otherwise start markdown. `all` escapes every
    /// ASCII punctuation character (always valid CommonMark).
    static func escape(_ s: String, at start: Int, in ctx: [Character], all: Bool, inLabel: Bool = false) -> String {
        var out = ""
        for (k, c) in s.enumerated() {
            let i = start + k
            let prev: Character? = i > 0 ? ctx[i - 1] : nil
            let next: Character? = i + 1 < ctx.count ? ctx[i + 1] : nil
            if c == "\n" { out.append(c); continue }
            if all {
                if isASCIIPunct(c) { out.append("\\") }
                out.append(c)
                continue
            }
            var esc = false
            switch c {
            case "\\":
                esc = next == nil || next == "\n" || next.map(isASCIIPunct) == true
            case "*", "`":
                esc = true
            case "_":
                esc = !(prev.map { $0.isLetter || $0.isNumber } == true && next.map { $0.isLetter || $0.isNumber } == true)
            case "[":
                esc = inLabel || next == "[" || prev == "["
            case "]":
                esc = inLabel || next == "(" || next == "[" || next == "]" || next == ":" || prev == "]"
            case "<":
                esc = next.map { $0.isLetter || $0 == "/" || $0 == "!" || $0 == "?" } == true
            case "&":
                esc = next == "#" || entityFollows(ctx, from: i + 1)
            case "~":
                esc = ctx[(i + 1)...].contains("~") || ctx[..<i].contains("~")
            case "!":
                esc = false
            default:
                break
            }
            if !esc, atLineStart(ctx, i) { esc = startsBlockSyntax(ctx, i) }
            if !esc, c == "." || c == ")", orderedMarkerBefore(ctx, i) { esc = true }
            if esc { out.append("\\") }
            out.append(c)
        }
        return out
    }

    static func entityFollows(_ ctx: [Character], from i: Int) -> Bool {
        var j = i
        while j < ctx.count, ctx[j].isASCII, ctx[j].isLetter || ctx[j].isNumber { j += 1 }
        return j > i && j < ctx.count && ctx[j] == ";"
    }

    /// Whether `i` is the first non-space character of a line.
    static func atLineStart(_ ctx: [Character], _ i: Int) -> Bool {
        var j = i - 1
        while j >= 0, ctx[j] == " " { j -= 1 }
        return j < 0 || ctx[j] == "\n"
    }

    /// Block syntax a paragraph line must not start with: `#`, `>`, a
    /// bullet, a setext/thematic line, a fence, a table pipe line.
    static func startsBlockSyntax(_ ctx: [Character], _ i: Int) -> Bool {
        let c = ctx[i]
        let next: Character? = i + 1 < ctx.count ? ctx[i + 1] : nil
        let lineEnd = ctx[i...].firstIndex(of: "\n") ?? ctx.count
        let line = ctx[i..<lineEnd]
        switch c {
        case "#", ">":
            return true
        case "-", "+":
            return next == nil || next == " " || next == "\t" || next == "\n" || line.allSatisfy { $0 == c || $0 == " " }
        case "=":
            return line.allSatisfy { $0 == "=" || $0 == " " }
        case "|":
            return true
        default:
            return false
        }
    }

    /// `1.` / `1)` at a line start: escape the delimiter.
    static func orderedMarkerBefore(_ ctx: [Character], _ i: Int) -> Bool {
        var j = i - 1
        var digits = 0
        while j >= 0, ctx[j].isASCII, ctx[j].isNumber { j -= 1; digits += 1 }
        guard (1...9).contains(digits), atLineStart(ctx, j + 1) else { return false }
        let next: Character? = i + 1 < ctx.count ? ctx[i + 1] : nil
        return next == nil || next == " " || next == "\t" || next == "\n"
    }
}

extension AttributedString {
    /// UTF-16 length, as UIKit counts.
    public var utf16Count: Int { String(characters).utf16.count }

    /// The index `offset` UTF-16 units in (clamped; never inside a character).
    public func index(utf16 offset: Int) -> AttributedString.Index {
        let s = String(characters)
        let clamped = Swift.max(0, Swift.min(offset, s.utf16.count))
        let si = String.Index(utf16Offset: clamped, in: s)
        let n = s.distance(from: s.startIndex, to: si < s.endIndex ? s.rangeOfComposedCharacterSequence(at: si).lowerBound : si)
        return characters.index(startIndex, offsetBy: n)
    }

    /// Split at a UTF-16 offset.
    public func split(utf16 offset: Int) -> (AttributedString, AttributedString) {
        let i = index(utf16: offset)
        return (AttributedString(self[startIndex..<i]), AttributedString(self[i..<endIndex]))
    }
}
