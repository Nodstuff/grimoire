import Foundation

/// The one-surface editor's text model: a whole doc as ONE attributed
/// string, the way the app's single `UITextView` holds it. Pure Foundation
/// (no UIKit), so every rule here runs under `swift test`; the app only adds
/// the look (fonts, colours, attachments) on top of these attributes.
///
/// Layout: one line per paragraph, heading, quote, list item or non-text
/// block, each ended by "\n" (the string always ends with one, so every
/// line has a terminator). A soft line break inside a block is U+2028. A
/// non-text block (code, diagram, table, callout, anything `EditorBlockContent`
/// keeps raw) is one U+FFFC character carrying its markdown in `raw`.
///
/// Every character carries two hidden attributes: `block` (the block id)
/// and `line` (the line's kind). A line's identity is its TERMINATOR's
/// attributes: typing anywhere in a line, even into an empty one, can't
/// change which block it is. The edits that join lines (Backspace at a
/// line's start, a deletion or typing across lines, Return, paste) are made
/// here, explicitly: the joined line keeps the FIRST line's id, a split's
/// new half gets a fresh one (diff.ts semantics: the first half keeps the
/// block). Consecutive list-item lines with one id are one list block.
/// New blocks get a client-minted id at once: an id the baseline doesn't
/// have saves as an insert with that id, so nothing needs stamping later.
public enum Surface {
    // MARK: attributes

    public static let block = NSAttributedString.Key("taisce.block")
    public static let line = NSAttributedString.Key("taisce.line")
    /// a non-text block's markdown, on its U+FFFC
    public static let raw = NSAttributedString.Key("taisce.raw")
    public static let marks = NSAttributedString.Key("taisce.marks")
    public static let link = NSAttributedString.Key("taisce.link")
    public static let wiki = NSAttributedString.Key("taisce.wiki")
    /// a list item's visible marker ("•\t", "1.\t", "☐\t"): never text
    public static let marker = NSAttributedString.Key("taisce.marker")

    public static let attachment = "\u{FFFC}"
    public static let softBreak = "\u{2028}"
    static let newline: unichar = 10
    static let objectChar: unichar = 0xFFFC

    public static func newID() -> BlockID { UUID().uuidString.lowercased() }

    /// The characters without copying them (a text storage's own string;
    /// `.string` bridges a copy, O(n) per call).
    static func text(_ s: NSAttributedString) -> NSString {
        if let m = s as? NSMutableAttributedString { return m.mutableString }
        return s.string as NSString
    }

    /// What a line is.
    public enum LineStyle: Hashable, Sendable {
        case paragraph
        case heading(Int)
        case quote
        case item(ListPrefix)
        case raw

        public var encoded: String {
            switch self {
            case .paragraph: "p"
            case let .heading(n): "h\(n)"
            case .quote: "q"
            case let .item(p): "l" + Surface.encode(p)
            case .raw: "r"
            }
        }

        public init(encoded s: String) {
            switch s.first {
            case "h": self = .heading(Int(s.dropFirst()) ?? 1)
            case "q": self = .quote
            case "r": self = .raw
            case "l": self = Surface.decode(String(s.dropFirst())).map { .item($0) } ?? .paragraph
            default: self = .paragraph
            }
        }

        public var isItem: Bool { if case .item = self { true } else { false } }
        public var prefix: ListPrefix? { if case let .item(p) = self { p } else { nil } }
    }

    static func encode(_ p: ListPrefix) -> String {
        "\(p.indent)|\(p.ordered ? 1 : 0)|\(p.marker)|\(p.number)|\(p.checkbox.map { $0 ? "1" : "0" } ?? "-")"
    }

    static func decode(_ s: String) -> ListPrefix? {
        let f = s.split(separator: "|", omittingEmptySubsequences: false)
        guard f.count == 5, let indent = Int(f[0]), let n = Int(f[3]), let m = f[2].first else { return nil }
        return ListPrefix(indent: indent, ordered: f[1] == "1", marker: m, number: n, checkbox: f[4] == "-" ? nil : f[4] == "1")
    }

    /// The marker text drawn before a list item.
    public static func markerText(_ p: ListPrefix) -> String {
        if let box = p.checkbox { return (box ? "☑" : "☐") + "\t" }
        if p.ordered { return "\(p.number)\(p.marker)\t" }
        return (p.indent % 2 == 0 ? "•" : "◦") + "\t"
    }

    // MARK: lines and blocks

    public struct Line: Hashable, Sendable {
        /// the whole line, terminator included (when it has one)
        public var range: NSRange
        public var terminated: Bool
        public var id: BlockID?
        public var style: LineStyle
        /// the marker's length at the line's start (list items)
        public var markerLength: Int

        public var end: Int { range.location + range.length }
        /// without the terminator
        public var contentRange: NSRange { NSRange(location: range.location, length: range.length - (terminated ? 1 : 0)) }
        /// without the marker or the terminator
        public var textRange: NSRange {
            let c = contentRange
            return NSRange(location: c.location + markerLength, length: max(0, c.length - markerLength))
        }
    }

    public static func lines(_ s: NSAttributedString) -> [Line] {
        let ns = text(s)
        var out: [Line] = []
        var start = 0
        let n = ns.length
        while start < n {
            let nl = ns.range(of: "\n", options: .literal, range: NSRange(location: start, length: n - start))
            let terminated = nl.location != NSNotFound
            let end = terminated ? nl.location + 1 : n
            let r = NSRange(location: start, length: end - start)
            let probe = terminated ? nl.location : start
            let id = s.attribute(block, at: probe, effectiveRange: nil) as? String
            var style = (s.attribute(line, at: probe, effectiveRange: nil) as? String).map(LineStyle.init(encoded:)) ?? .paragraph
            if !terminated, r.length == 0 { style = .paragraph }
            var markerLength = 0
            if style.isItem {
                var i = start
                while i < end - (terminated ? 1 : 0), s.attribute(marker, at: i, effectiveRange: nil) != nil { i += 1 }
                markerLength = i - start
            }
            out.append(Line(range: r, terminated: terminated, id: id, style: style, markerLength: markerLength))
            start = end
        }
        return out
    }

    /// The index of the line holding `location` (a location at a line's
    /// end, past its terminator, is the next line's start).
    public static func lineIndex(_ ls: [Line], at location: Int) -> Int {
        guard !ls.isEmpty else { return 0 }
        var lo = 0, hi = ls.count - 1
        while lo < hi {
            let mid = (lo + hi + 1) / 2
            if ls[mid].range.location <= location { lo = mid } else { hi = mid - 1 }
        }
        return lo
    }

    /// One block on the surface: consecutive lines sharing an id.
    public struct SurfaceBlock: Hashable, Sendable {
        public var id: BlockID
        /// line indices
        public var lines: Range<Int>
        public var range: NSRange
        public var content: EditorBlockContent

        public var level: Int { if case let .heading(l, _) = content { l } else { 0 } }
    }

    public static func blocks(_ s: NSAttributedString, lines ls: [Line]? = nil) -> [SurfaceBlock] {
        let ls = ls ?? lines(s)
        var out: [SurfaceBlock] = []
        var i = 0
        while i < ls.count {
            var j = i + 1
            if ls[i].style.isItem, let id = ls[i].id {
                while j < ls.count, ls[j].id == id, ls[j].style.isItem { j += 1 }
            }
            let range = NSRange(location: ls[i].range.location, length: ls[j - 1].end - ls[i].range.location)
            out.append(SurfaceBlock(id: ls[i].id ?? "", lines: i..<j, range: range, content: content(s, ls[i..<j])))
            i = j
        }
        return out
    }

    static func content(_ s: NSAttributedString, _ ls: ArraySlice<Line>) -> EditorBlockContent {
        let first = ls[ls.startIndex]
        switch first.style {
        case .paragraph: return .paragraph(inline(s, first.textRange))
        case let .heading(n): return .heading(level: n, inline(s, first.textRange))
        case .quote: return .quote(inline(s, first.textRange))
        case .item:
            return .list(ls.map { EditorListItem($0.style.prefix ?? .bullet, inline(s, $0.textRange)) })
        case .raw:
            let c = first.contentRange
            if c.length > 0, let src = s.attribute(raw, at: c.location, effectiveRange: nil) as? String { return .raw(src) }
            return .raw("")
        }
    }

    /// A line's text as the model holds it (soft breaks back to "\n").
    public static func inline(_ s: NSAttributedString, _ range: NSRange) -> AttributedString {
        guard range.length > 0 else { return AttributedString() }
        var runs: [InlineRun] = []
        s.enumerateAttributes(in: range) { a, r, _ in
            guard a[marker] == nil else { return }
            var text = text(s).substring(with: r)
            text = text.replacingOccurrences(of: softBreak, with: "\n").replacingOccurrences(of: attachment, with: "")
            guard !text.isEmpty else { return }
            runs.append(InlineRun(text, marks: InlineMarks(rawValue: (a[marks] as? Int) ?? 0), link: a[link] as? String, wiki: a[wiki] as? String))
        }
        return InlineCodec.build(runs)
    }

    // MARK: rendering

    /// Lines for one block, model attributes only.
    public static func render(_ content: EditorBlockContent, id: BlockID) -> NSMutableAttributedString {
        let out = NSMutableAttributedString()
        func base(_ style: LineStyle) -> [NSAttributedString.Key: Any] { [block: id, line: style.encoded] }
        func text(_ t: AttributedString, _ style: LineStyle) {
            for r in InlineCodec.runs(t) {
                var a = base(style)
                if !r.marks.isEmpty { a[marks] = r.marks.rawValue }
                if let l = r.link { a[link] = l }
                if let w = r.wiki { a[wiki] = w }
                out.append(NSAttributedString(string: r.text.replacingOccurrences(of: "\n", with: softBreak), attributes: a))
            }
            out.append(NSAttributedString(string: "\n", attributes: base(style)))
        }
        switch content {
        case let .paragraph(t): text(t, .paragraph)
        case let .heading(level, t): text(EditorBlockContent.singleLine(t), .heading(max(1, min(6, level))))
        case let .quote(t): text(t, .quote)
        case let .list(items):
            for item in EditorCommands.renumbered(items) {
                var m = base(.item(item.prefix))
                m[marker] = encode(item.prefix)
                out.append(NSAttributedString(string: markerText(item.prefix), attributes: m))
                text(EditorBlockContent.singleLine(item.text), .item(item.prefix))
            }
        case let .raw(src):
            var a = base(.raw)
            a[raw] = src
            out.append(NSAttributedString(string: attachment, attributes: a))
            out.append(NSAttributedString(string: "\n", attributes: base(.raw)))
        }
        return out
    }

    /// A whole doc. Empty → one empty paragraph with a fresh id.
    public static func document(_ blocks: [(id: BlockID, content: EditorBlockContent)], mint: () -> BlockID = newID) -> NSMutableAttributedString {
        let out = NSMutableAttributedString()
        for b in blocks { out.append(render(b.content, id: b.id)) }
        if out.length == 0 { out.append(render(.paragraph(AttributedString()), id: mint())) }
        return out
    }

    /// Where `caret` falls inside `content` as `render` lays it out.
    public static func location(of caret: EditorCaret, in content: EditorBlockContent) -> Int {
        switch content {
        case let .list(items):
            let items = EditorCommands.renumbered(items)
            var loc = 0
            for (i, item) in items.enumerated() {
                let len = (markerText(item.prefix) as NSString).length
                if i == min(max(0, caret.line), items.count - 1) {
                    return loc + len + min(max(0, caret.offset), item.text.utf16Count)
                }
                loc += len + EditorBlockContent.singleLine(item.text).utf16Count + 1
            }
            return loc
        case .raw:
            return caret.offset > 0 ? 1 : 0
        default:
            return min(max(0, caret.offset), content.inline.utf16Count)
        }
    }

    /// The caret at `location` as a block-relative `EditorCaret`.
    public static func caret(at location: Int, in b: SurfaceBlock, lines ls: [Line]) -> EditorCaret {
        let li = min(max(lineIndex(ls, at: location), b.lines.lowerBound), b.lines.upperBound - 1)
        let l = ls[li]
        let off = max(0, min(location, l.textRange.location + l.textRange.length) - l.textRange.location)
        return EditorCaret(line: li - b.lines.lowerBound, offset: off)
    }

    // MARK: edits

    /// A replacement of `range` in the current string, and where the
    /// selection goes afterwards (in the new string).
    public struct Edit: Equatable, @unchecked Sendable {
        public var range: NSRange
        public var replacement: NSAttributedString
        public var selection: NSRange

        public init(range: NSRange, replacement: NSAttributedString, selection: NSRange) {
            self.range = range
            self.replacement = replacement
            self.selection = selection
        }

        /// Only move the selection.
        public static func select(_ r: NSRange) -> Edit { Edit(range: NSRange(location: r.location, length: 0), replacement: NSAttributedString(), selection: r) }

        public var isSelectionOnly: Bool { range.length == 0 && replacement.length == 0 }

        public func apply(to s: NSMutableAttributedString) {
            guard !isSelectionOnly else { return }
            s.replaceCharacters(in: range, with: replacement)
        }
    }

    /// Blocks (in order) replacing block `b`; the caret goes to piece `focus`.
    static func blockEdit(_ b: SurfaceBlock, pieces: [(id: BlockID, content: EditorBlockContent)], focus: Int, caret: EditorCaret) -> Edit {
        let out = NSMutableAttributedString()
        var sel = b.range.location
        for (i, p) in pieces.enumerated() {
            if i == focus { sel = b.range.location + out.length + Surface.location(of: caret, in: p.content) }
            out.append(render(p.content, id: p.id))
        }
        return Edit(range: b.range, replacement: out, selection: NSRange(location: sel, length: 0))
    }

    /// What an `EditorCommands` result does to block `b`.
    static func edit(for result: BlockCommandResult, block b: SurfaceBlock, mint: () -> BlockID) -> Edit? {
        switch result {
        case .none, .mergeWithPrevious:
            return nil
        case let .update(c, caret):
            return blockEdit(b, pieces: [(b.id, c)], focus: 0, caret: caret)
        case let .split(current, inserted, focus, caret):
            let pieces = [(id: b.id, content: current)] + inserted.map { (id: mint(), content: $0) }
            let f = focus == 0 || inserted.isEmpty ? 0 : min(focus, inserted.count)
            return blockEdit(b, pieces: pieces, focus: f, caret: caret)
        case let .insertBefore(c):
            return blockEdit(b, pieces: [(mint(), c), (b.id, b.content)], focus: 1, caret: .start)
        }
    }

    static func block(_ bs: [SurfaceBlock], line li: Int) -> Int? {
        bs.firstIndex { $0.lines.contains(li) }
    }

    /// Return with the caret at `location` (the selection already deleted).
    public static func returnKey(_ s: NSAttributedString, at location: Int, mint: () -> BlockID = newID) -> Edit? {
        let ls = lines(s)
        guard !ls.isEmpty else { return nil }
        let li = lineIndex(ls, at: location)
        let bs = blocks(s, lines: ls)
        guard let bi = block(bs, line: li) else { return nil }
        let b = bs[bi]
        if case .raw = b.content {
            // beside a non-text block: a new paragraph before or after it
            let empty = render(.paragraph(AttributedString()), id: mint())
            if location <= b.range.location {
                return Edit(range: NSRange(location: b.range.location, length: 0), replacement: empty,
                            selection: NSRange(location: b.range.location + empty.length, length: 0))
            }
            let end = b.range.location + b.range.length
            return Edit(range: NSRange(location: end, length: 0), replacement: empty, selection: NSRange(location: end, length: 0))
        }
        return edit(for: EditorCommands.returnKey(b.content, at: Surface.caret(at: location, in: b, lines: ls)), block: b, mint: mint)
    }

    /// Backspace with an empty selection at `location`. Nil = not at a
    /// line's start: the text view deletes a character as usual.
    public static func backspace(_ s: NSAttributedString, at location: Int, mint: () -> BlockID = newID) -> Edit? {
        let ls = lines(s)
        guard !ls.isEmpty else { return nil }
        let li = lineIndex(ls, at: location)
        let l = ls[li]
        let bs = blocks(s, lines: ls)
        guard let bi = block(bs, line: li) else { return nil }
        let b = bs[bi]
        let prev = bi > 0 ? bs[bi - 1] : nil
        if case .raw = b.content {
            guard location <= l.range.location else { return nil }
            return joinBackward(b, prev: prev)
        }
        guard location == l.textRange.location else { return nil }
        let caret = EditorCaret(line: li - b.lines.lowerBound, offset: 0)
        let result = EditorCommands.backspaceAtStart(b.content, at: caret)
        guard case .mergeWithPrevious = result else { return edit(for: result, block: b, mint: mint) }
        guard let prev else { return .select(NSRange(location: location, length: 0)) }
        if case .raw = prev.content {
            // an empty line after a non-text block goes; otherwise select
            // the block, so a second Backspace deletes it
            if b.content.isEmpty {
                return Edit(range: b.range, replacement: NSAttributedString(), selection: NSRange(location: prev.range.location + 1, length: 0))
            }
            return .select(NSRange(location: prev.range.location, length: 1))
        }
        if prev.content.isEmpty, case .paragraph = prev.content {
            // an empty paragraph above just goes; this block keeps its id
            return Edit(range: prev.range, replacement: NSAttributedString(), selection: NSRange(location: prev.range.location, length: 0))
        }
        guard let (merged, at) = EditorCommands.merge(prev.content, b.content) else {
            return .select(NSRange(location: prev.range.location + prev.range.length - 1, length: 0))
        }
        let out = render(merged, id: prev.id)
        let range = NSRange(location: prev.range.location, length: b.range.location + b.range.length - prev.range.location)
        return Edit(range: range, replacement: out, selection: NSRange(location: prev.range.location + Surface.location(of: at, in: merged), length: 0))
    }

    /// Backspace in front of a non-text block.
    static func joinBackward(_ b: SurfaceBlock, prev: SurfaceBlock?) -> Edit {
        guard let prev else { return .select(NSRange(location: b.range.location, length: 0)) }
        if prev.content.isEmpty, !prev.content.isRaw {
            return Edit(range: prev.range, replacement: NSAttributedString(), selection: NSRange(location: prev.range.location, length: 0))
        }
        if case .raw = prev.content { return .select(NSRange(location: prev.range.location, length: 1)) }
        return .select(NSRange(location: prev.range.location + prev.range.length - 1, length: 0))
    }

    /// Markdown typed at a block's start (`# `, `- `, `1. `, `[ ] `, `> `),
    /// checked after each keystroke with the caret right after it.
    public static func shortcut(_ s: NSAttributedString, at location: Int) -> Edit? {
        let ls = lines(s)
        guard !ls.isEmpty else { return nil }
        let li = lineIndex(ls, at: location)
        let bs = blocks(s, lines: ls)
        guard let bi = block(bs, line: li) else { return nil }
        let b = bs[bi]
        switch b.content {
        case .paragraph, .list: break
        default: return nil
        }
        guard let (c, caret) = EditorCommands.shortcut(b.content, caret: Surface.caret(at: location, in: b, lines: ls)) else { return nil }
        return blockEdit(b, pieces: [(b.id, c)], focus: 0, caret: caret)
    }

    /// A block-level change at `location` (kind, to-do, indent): `f` gets
    /// the block and the caret in it.
    public static func transform(_ s: NSAttributedString, at location: Int, mint: () -> BlockID = newID,
                                 _ f: (EditorBlockContent, EditorCaret) -> BlockCommandResult) -> Edit? {
        let ls = lines(s)
        guard !ls.isEmpty else { return nil }
        let li = lineIndex(ls, at: location)
        let bs = blocks(s, lines: ls)
        guard let bi = block(bs, line: li) else { return nil }
        return edit(for: f(bs[bi].content, Surface.caret(at: location, in: bs[bi], lines: ls)), block: bs[bi], mint: mint)
    }

    /// Tick or untick list line `li`'s box; the selection stays.
    public static func setChecked(_ s: NSAttributedString, line li: Int, _ checked: Bool, selection: NSRange) -> Edit? {
        let ls = lines(s)
        guard ls.indices.contains(li), ls[li].style.prefix?.checkbox != nil else { return nil }
        let bs = blocks(s, lines: ls)
        guard let bi = block(bs, line: li) else { return nil }
        let b = bs[bi]
        let c = EditorCommands.setChecked(b.content, item: li - b.lines.lowerBound, checked)
        guard c != b.content else { return nil }
        let out = render(c, id: b.id)
        return Edit(range: b.range, replacement: out, selection: selection)
    }

    /// Replace one block's content (a non-text block's source edited in
    /// its sheet, a remote change). The selection is kept, shifted when
    /// it sits after the block.
    public static func replaceBlock(_ s: NSAttributedString, id: BlockID, with content: EditorBlockContent, selection: NSRange) -> Edit? {
        let bs = blocks(s)
        guard let b = bs.first(where: { $0.id == id }) else { return nil }
        let out = render(content, id: id)
        return Edit(range: b.range, replacement: out, selection: shifted(selection, by: b.range, newLength: out.length))
    }

    /// `sel` after `range` is replaced by `newLength` characters.
    public static func shifted(_ sel: NSRange, by range: NSRange, newLength: Int) -> NSRange {
        let end = range.location + range.length
        let delta = newLength - range.length
        if sel.location >= end { return NSRange(location: sel.location + delta, length: sel.length) }
        if sel.location + sel.length <= range.location { return sel }
        // inside the replaced range: clamp into the new text
        let loc = min(max(sel.location, range.location), range.location + max(0, newLength - 1))
        return NSRange(location: loc, length: 0)
    }

    /// Model attributes a typed character carries, for `typingAttributes`.
    public static func typingAttributes(_ s: NSAttributedString, at location: Int) -> [NSAttributedString.Key: Any] {
        let ls = lines(s)
        guard !ls.isEmpty else { return [:] }
        let l = ls[lineIndex(ls, at: location)]
        var a: [NSAttributedString.Key: Any] = [line: l.style.encoded]
        if let id = l.id { a[block] = id }
        if location > l.textRange.location, location - 1 < s.length {
            let before = s.attributes(at: location - 1, effectiveRange: nil)
            if before[marker] == nil, text(s).character(at: location - 1) != newline {
                if let m = before[marks] as? Int, m != 0 { a[marks] = m }
                // a link continues only inside it
                if let lk = before[link] as? String, location < s.length, (s.attribute(link, at: location, effectiveRange: nil) as? String) == lk {
                    a[link] = lk
                }
            }
        }
        return a
    }

    /// Replace `range` with plain `text` (typed, pasted, or a deletion
    /// across lines), keeping the block model straight: the line the range
    /// starts in keeps its id and kind; lines the text adds are new
    /// paragraphs (new items of the same list, in a list).
    public static func replace(_ s: NSAttributedString, _ range: NSRange, with text: String,
                               typing: [NSAttributedString.Key: Any] = [:], mint: () -> BlockID = newID) -> Edit {
        let ls = lines(s)
        guard !ls.isEmpty else {
            let out = NSMutableAttributedString(string: text + "\n", attributes: [block: mint(), line: LineStyle.paragraph.encoded])
            return Edit(range: NSRange(location: 0, length: s.length), replacement: out, selection: NSRange(location: (text as NSString).length, length: 0))
        }
        let first = lineIndex(ls, at: range.location)
        var last = max(first, lineIndex(ls, at: max(range.location, range.location + range.length - 1)))
        let end = range.location + range.length
        if range.length > 0, end < s.length, Surface.text(s).character(at: end - 1) == newline {
            // the range takes a line's terminator: the next line joins this one
            last = max(last, lineIndex(ls, at: end))
        }
        let span = NSRange(location: ls[first].range.location, length: ls[last].end - ls[first].range.location)
        let sub = NSMutableAttributedString(attributedString: s.attributedSubstring(from: span))
        let local = NSRange(location: range.location - span.location, length: range.length)
        var attrs = typing
        for k in [block, line, marker, wiki, raw] { attrs[k] = nil }
        let typed = text.replacingOccurrences(of: "\r\n", with: "\n").replacingOccurrences(of: "\r", with: "\n")
        sub.replaceCharacters(in: local, with: NSAttributedString(string: typed, attributes: attrs))
        // the joined line is the first line's, unless that was a whole
        // non-text block (deleted as a unit): then the line after it
        let keeper = ls[first].style == .raw && range.location == ls[first].range.location && last > first ? last : first
        let aID = ls[keeper].id ?? mint()
        let aStyle = ls[keeper].style
        var subLines = lines(sub)
        // an unterminated tail (the range ran to the doc's end): terminate it
        if let tail = subLines.last, !tail.terminated {
            sub.append(NSAttributedString(string: "\n"))
            subLines = lines(sub)
        }
        for (i, l) in subLines.enumerated() {
            var id = aID
            var style = aStyle
            if i > 0 {
                if case let .item(p) = aStyle {
                    style = .item(p.continued)
                } else {
                    id = mint()
                    style = .paragraph
                }
            }
            sub.addAttributes([block: id, line: style.encoded], range: l.range)
            if i > 0 || !style.isItem {
                // markers are rebuilt by `normalize`; text never becomes one
                sub.removeAttribute(marker, range: i == 0 ? NSRange(location: l.range.location + l.markerLength, length: l.range.length - l.markerLength) : l.range)
            }
        }
        let sel = span.location + local.location + (typed as NSString).length
        return Edit(range: span, replacement: sub, selection: NSRange(location: sel, length: 0))
    }

    /// A deletion that would leave half a non-text block: it takes the
    /// block's whole line.
    public static func expandDeletion(_ range: NSRange, in s: NSAttributedString) -> NSRange {
        guard range.length > 0 else { return range }
        let ns = text(s)
        let last = range.location + range.length - 1
        if ns.character(at: last) == objectChar, last + 1 < ns.length, ns.character(at: last + 1) == newline {
            return NSRange(location: range.location, length: range.length + 1)
        }
        return range
    }

    /// Wikilink runs overlapping `range` (an insertion point strictly
    /// inside one counts), as one range; nil when none.
    public static func expandToWikiLinks(_ range: NSRange, in s: NSAttributedString) -> NSRange? {
        guard s.length > 0 else { return nil }
        var lo = range.location, hi = range.location + range.length
        var touched = false
        func runAt(_ i: Int) -> NSRange? {
            guard i >= 0, i < s.length, s.attribute(wiki, at: i, effectiveRange: nil) != nil else { return nil }
            var eff = NSRange()
            _ = s.attribute(wiki, at: i, longestEffectiveRange: &eff, in: NSRange(location: 0, length: s.length))
            return eff
        }
        if range.length == 0 {
            if let a = runAt(range.location - 1), let b = runAt(range.location), a == b { return a }
            return nil
        }
        for i in [range.location, range.location + range.length - 1] {
            if let r = runAt(i) {
                touched = true
                lo = min(lo, r.location)
                hi = max(hi, r.location + r.length)
            }
        }
        return touched ? NSRange(location: lo, length: hi - lo) : nil
    }

    // MARK: normalize

    /// Restore the invariants after any edit (ours, the text view's, an
    /// undo): the string ends with "\n"; a non-text block is alone on its
    /// line; every character of a line carries the line's id and kind;
    /// an id names one block only (a later duplicate gets a fresh id); list
    /// markers are where they belong and numbered. `selection` follows the
    /// text. True when anything changed.
    @discardableResult
    public static func normalize(_ s: NSMutableAttributedString, selection: inout NSRange, around: NSRange? = nil,
                                 mint: () -> BlockID = newID) -> Bool {
        var changed = false
        func replace(_ r: NSRange, _ with: NSAttributedString) {
            s.replaceCharacters(in: r, with: with)
            selection = shifted(selection, by: r, newLength: with.length)
            changed = true
        }
        // 1. a final terminator
        if s.length == 0 {
            replace(NSRange(location: 0, length: 0), render(.paragraph(AttributedString()), id: mint()))
        } else if text(s).character(at: s.length - 1) != newline {
            let a = s.attributes(at: s.length - 1, effectiveRange: nil).filter { $0.key == block || $0.key == line }
            let keep = selection
            replace(NSRange(location: s.length, length: 0), NSAttributedString(string: "\n", attributes: a))
            selection = keep
        }
        // `around`: only the lines it touches can be out of shape (typing
        // in a line); nil: anywhere (an undo, a rebuild)
        var scope: NSRange?
        if let around {
            let ns = text(s)
            let lo = min(max(0, around.location), ns.length - 1)
            let hi = min(max(lo, around.location + around.length), ns.length - 1)
            let a = ns.paragraphRange(for: NSRange(location: max(0, lo - 1), length: 0))
            let b = ns.paragraphRange(for: NSRange(location: min(ns.length - 1, hi + 1), length: 0))
            scope = NSRange(location: a.location, length: b.location + b.length - a.location)
        }
        // 2. non-text blocks alone on their lines; stray U+FFFC go
        var ns = text(s)
        var searchEnd = scope.map { $0.location + $0.length } ?? ns.length
        let searchStart = scope?.location ?? 0
        while searchEnd > searchStart {
            let found = text(s).range(of: attachment, options: [.literal, .backwards],
                                      range: NSRange(location: searchStart, length: searchEnd - searchStart))
            guard found.location != NSNotFound else { break }
            let i = found.location
            searchEnd = i
            let src = s.attribute(raw, at: i, effectiveRange: nil) as? String
            if src == nil {
                replace(NSRange(location: i, length: 1), NSAttributedString())
            } else {
                let a = s.attributes(at: i, effectiveRange: nil)
                let id = (a[block] as? String) ?? mint()
                if i + 1 < s.length, text(s).character(at: i + 1) != newline {
                    replace(NSRange(location: i + 1, length: 0), NSAttributedString(string: "\n", attributes: [block: id, line: LineStyle.raw.encoded]))
                } else if i + 1 < s.length {
                    s.addAttributes([block: id, line: LineStyle.raw.encoded], range: NSRange(location: i + 1, length: 1))
                }
                if i > 0, text(s).character(at: i - 1) != newline {
                    let before = s.attributes(at: i - 1, effectiveRange: nil).filter { $0.key == block || $0.key == line }
                    replace(NSRange(location: i, length: 0), NSAttributedString(string: "\n", attributes: before))
                }
            }
        }
        if let sc = scope {
            // the scope grew by what step 2 inserted
            let grown = text(s).paragraphRange(for: NSRange(location: min(sc.location, max(0, s.length - 1)), length: 0))
            scope = NSRange(location: grown.location, length: max(0, min(s.length, sc.location + sc.length + (s.length - ns.length)) - grown.location))
        }
        ns = text(s)
        // 3. ids and kinds, line by line
        let ls = lines(s)
        var used: Set<BlockID> = []
        var prevOrig: BlockID?
        var prevAssigned: BlockID?
        var prevItem = false
        var plan: [(id: BlockID, style: LineStyle)] = []
        for l in ls {
            var style = l.style
            let c = l.contentRange
            let isObject = c.length == 1 && ns.character(at: c.location) == objectChar
            var orig = l.id
            if style == .raw, !isObject {
                // its block was deleted: what's left is new text
                style = .paragraph
                orig = nil
            } else if isObject {
                style = .raw
                orig = (s.attribute(block, at: c.location, effectiveRange: nil) as? String) ?? orig
            }
            let assigned: BlockID
            if style.isItem, prevItem, let o = orig, o == prevOrig, let p = prevAssigned {
                assigned = p
            } else if let o = orig, !used.contains(o) {
                assigned = o
            } else {
                assigned = mint()
            }
            used.insert(assigned)
            prevOrig = orig
            prevAssigned = assigned
            prevItem = style.isItem
            plan.append((assigned, style))
        }
        // ordered lists number from each block's first item
        var k = 0
        while k < plan.count {
            guard plan[k].style.isItem else { k += 1; continue }
            var j = k
            while j + 1 < plan.count, plan[j + 1].style.isItem, plan[j + 1].id == plan[k].id { j += 1 }
            let items = plan[k...j].map { EditorListItem($0.style.prefix!) }
            for (n, item) in EditorCommands.renumbered(items).enumerated() { plan[k + n].style = .item(item.prefix) }
            k = j + 1
        }
        // apply, last line first so ranges stay put
        for idx in ls.indices.reversed() {
            let l = ls[idx]
            let (id, style) = plan[idx]
            let want = [block: id, line: style.encoded] as [NSAttributedString.Key: Any]
            let inScope = scope.map { NSIntersectionRange($0, l.range).length > 0 || NSLocationInRange(l.range.location, $0) } ?? true
            // outside the scope only a line whose id or kind the plan changed needs work
            guard inScope || id != l.id || style != l.style else { continue }
            var uniform = true
            s.enumerateAttributes(in: l.range) { a, _, stop in
                if (a[block] as? String) != id || (a[line] as? String) != style.encoded { uniform = false; stop.pointee = true }
            }
            if !uniform {
                s.addAttributes(want, range: l.range)
                changed = true
            }
            // markers: only at an item's start, and the right text
            let c = l.contentRange
            var strays: [NSRange] = []
            s.enumerateAttribute(marker, in: c) { v, r, _ in
                guard v != nil else { return }
                if style.isItem, r.location == c.location { return }
                strays.append(r)
            }
            for r in strays.reversed() { replace(r, NSAttributedString()) }
            if case let .item(p) = style {
                var lead = 0
                while l.range.location + lead < s.length, text(s).character(at: l.range.location + lead) != newline,
                      s.attribute(marker, at: l.range.location + lead, effectiveRange: nil) != nil { lead += 1 }
                let current = text(s).substring(with: NSRange(location: l.range.location, length: lead))
                let enc = encode(p)
                let markerOK = current == markerText(p) && lead > 0 && (s.attribute(marker, at: l.range.location, effectiveRange: nil) as? String) == enc
                if !markerOK {
                    var m = want
                    m[marker] = enc
                    replace(NSRange(location: l.range.location, length: lead), NSAttributedString(string: markerText(p), attributes: m))
                }
            }
        }

        // the caret never sits inside a marker or past the final "\n"
        if selection.location + selection.length > s.length - 1 || selection.location > s.length - 1 {
            let loc = min(selection.location, max(0, s.length - 1))
            selection = NSRange(location: loc, length: max(0, min(selection.length, s.length - 1 - loc)))
        }
        return changed
    }

    // MARK: saving

    /// The doc's entries for `DocDiff.computeOps`. An empty paragraph the
    /// server doesn't have yet is not a block (as on the web).
    public static func entries(_ s: NSAttributedString, known: Set<BlockID>) -> [DocDiff.Entry] {
        blocks(s).compactMap { b in
            if case .paragraph = b.content, b.content.isEmpty, !known.contains(b.id) { return nil }
            return DocDiff.Entry(id: b.id, content: b.content.markdown, level: b.level)
        }
    }

    /// The baseline form of a server block: what the surface would write
    /// for it untouched (so an untouched block is never written).
    public static func baselineContent(_ b: Block) -> String {
        EditorBlockContent.parse(b).markdown
    }

    /// `is_editor_hidden` (crates/store/src/lib.rs): comments, canvases,
    /// frontmatter stay out of the surface (and so out of the baseline).
    public static func isHidden(_ b: Block) -> Bool {
        b.blockType == .comment || b.blockType == .canvasScene || b.content.hasPrefix("---")
    }

    /// The surface's blocks and baseline for an editor's view of a doc.
    public static func load(_ editor: DocEditor) -> (blocks: [(id: BlockID, content: EditorBlockContent)], baseline: [DocDiff.BaselineBlock]) {
        var blocks: [(id: BlockID, content: EditorBlockContent)] = []
        var baseline: [DocDiff.BaselineBlock] = []
        for (b, _) in editor.ordered() where !isHidden(b) {
            let c = EditorBlockContent.parse(b)
            blocks.append((b.id, c))
            baseline.append(DocDiff.BaselineBlock(id: b.id, content: c.markdown, parent: b.parentID, orderKey: b.orderKey))
        }
        return (blocks, baseline)
    }
}
