import Foundation

/// A caret inside one block: for lists, the item and the UTF-16 offset in
/// its text; for every other kind, line 0 and the offset in the block's text.
public struct EditorCaret: Hashable, Sendable {
    public var line: Int
    public var offset: Int

    public init(line: Int = 0, offset: Int) {
        self.line = line
        self.offset = offset
    }

    public static let start = EditorCaret(offset: 0)
}

/// What a key press does to one block. `inserted` blocks go right after
/// it in document order; `focus` 0 is this block, n is `inserted[n - 1]`.
public enum BlockCommandResult: Hashable, Sendable {
    /// the block's own content changed (a new list item, an outdent)
    case update(EditorBlockContent, caret: EditorCaret)
    case split(current: EditorBlockContent, inserted: [EditorBlockContent], focus: Int, caret: EditorCaret)
    /// a new empty paragraph before this block; the caret stays put
    case insertBefore(EditorBlockContent)
    /// Backspace at the very start of a paragraph: join the previous block
    case mergeWithPrevious
    /// nothing to do
    case none
}

/// The kinds the block-type menu offers.
public enum BlockKindChoice: Hashable, Sendable, CaseIterable {
    case paragraph, heading1, heading2, heading3, bullet, numbered, todo, quote
}

/// Pure editing commands over one block: Return, Backspace at the start,
/// markdown shortcuts, list indentation, kind changes.
public enum EditorCommands {
    // MARK: Return

    public static func returnKey(_ content: EditorBlockContent, at caret: EditorCaret) -> BlockCommandResult {
        switch content {
        case let .paragraph(t):
            // ``` (or ```lang) alone: a code block
            let plain = String(t.characters)
            if let fence = codeFence(plain) {
                let raw = "\(fence)\n\n\(fence.prefix(3))"
                return .update(.raw(raw), caret: EditorCaret(offset: fence.utf16.count + 1))
            }
            return splitText(t, at: caret.offset, wrap: { .paragraph($0) }, next: { .paragraph($0) }, isEmpty: plain.isEmpty)

        case let .heading(level, t):
            // the text after the caret becomes the section's first paragraph
            return splitText(t, at: caret.offset, wrap: { .heading(level: level, $0) }, next: { .paragraph($0) }, isEmpty: t.characters.isEmpty)

        case let .quote(t):
            if t.characters.isEmpty { return .update(.paragraph(t), caret: .start) }
            return splitText(t, at: caret.offset, wrap: { .quote($0) }, next: { $0.characters.isEmpty ? .paragraph($0) : .quote($0) }, isEmpty: false)

        case var .list(items):
            let k = max(0, min(caret.line, items.count - 1))
            let item = items[k]
            if item.text.characters.isEmpty {
                // an empty item: outdent, or leave the list
                if item.prefix.indent > 0 { return outdent(content, at: caret) }
                return leaveList(items, at: k)
            }
            let (before, after) = item.text.split(utf16: caret.offset)
            if caret.offset == 0 {
                // Return at the start of an item: an empty item above it
                items.insert(EditorListItem(item.prefix, AttributedString()), at: k)
                return .update(.list(renumbered(items)), caret: EditorCaret(line: k + 1, offset: 0))
            }
            items[k].text = before
            items.insert(EditorListItem(item.prefix.continued, after), at: k + 1)
            return .update(.list(renumbered(items)), caret: EditorCaret(line: k + 1, offset: 0))

        case .raw:
            return .none
        }
    }

    /// "```" / "```swift" / "~~~" typed alone.
    static func codeFence(_ s: String) -> String? {
        let t = s.trimmingCharacters(in: .whitespaces)
        for f in ["```", "~~~"] where t.hasPrefix(f) {
            let lang = t.dropFirst(3)
            if lang.allSatisfy({ $0.isLetter || $0.isNumber || "+-_.#".contains($0) }), !lang.contains(f.first!) {
                return t
            }
        }
        return nil
    }

    static func splitText(
        _ t: AttributedString, at offset: Int,
        wrap: (AttributedString) -> EditorBlockContent, next: (AttributedString) -> EditorBlockContent,
        isEmpty: Bool
    ) -> BlockCommandResult {
        if offset == 0, !isEmpty {
            return .insertBefore(.paragraph(AttributedString()))
        }
        var (before, after) = t.split(utf16: offset)
        // a soft line break at the cut belongs to neither half
        if before.characters.last == "\n" { before = before.split(utf16: before.utf16Count - 1).0 }
        if after.characters.first == "\n" { after = after.split(utf16: 1).1 }
        return .split(current: wrap(before), inserted: [next(after)], focus: 1, caret: .start)
    }

    /// The empty item `k` becomes an empty paragraph between the items
    /// before and after it.
    static func leaveList(_ items: [EditorListItem], at k: Int) -> BlockCommandResult {
        let before = Array(items[..<k])
        let after = Array(items[(k + 1)...])
        let paragraph = EditorBlockContent.paragraph(AttributedString())
        if before.isEmpty {
            return .split(current: paragraph, inserted: after.isEmpty ? [] : [.list(rebased(after))], focus: 0, caret: .start)
        }
        var inserted: [EditorBlockContent] = [paragraph]
        if !after.isEmpty { inserted.append(.list(rebased(after))) }
        return .split(current: .list(before), inserted: inserted, focus: 1, caret: .start)
    }

    // MARK: Backspace at the start

    /// Backspace with the caret at offset 0 of `caret.line`.
    public static func backspaceAtStart(_ content: EditorBlockContent, at caret: EditorCaret) -> BlockCommandResult {
        switch content {
        case .paragraph:
            return .mergeWithPrevious
        case let .heading(_, t), let .quote(t):
            // a heading or quote first turns back into text
            return .update(.paragraph(t), caret: .start)
        case let .list(items):
            let k = max(0, min(caret.line, items.count - 1))
            if items[k].prefix.indent > 0 { return outdent(content, at: caret) }
            // the item becomes a paragraph, splitting the list around it
            let before = Array(items[..<k])
            let after = Array(items[(k + 1)...])
            let paragraph = EditorBlockContent.paragraph(items[k].text)
            if before.isEmpty {
                return .split(current: paragraph, inserted: after.isEmpty ? [] : [.list(rebased(after))], focus: 0, caret: .start)
            }
            var inserted: [EditorBlockContent] = [paragraph]
            if !after.isEmpty { inserted.append(.list(rebased(after))) }
            return .split(current: .list(before), inserted: inserted, focus: 1, caret: .start)
        case let .raw(s):
            return s.isEmpty ? .mergeWithPrevious : .none
        }
    }

    /// The previous block's content with `next` joined onto its end, and
    /// where the caret goes (the join point). Nil when the previous block
    /// can't take it (raw source): the caret just moves there.
    public static func merge(_ previous: EditorBlockContent, _ next: EditorBlockContent) -> (EditorBlockContent, EditorCaret)? {
        let tail: AttributedString
        switch next {
        case .raw(let s) where s.isEmpty: tail = AttributedString()
        case .raw: return nil
        default: tail = next.inline
        }
        switch previous {
        case let .paragraph(t):
            return (.paragraph(t + tail), EditorCaret(offset: t.utf16Count))
        case let .heading(level, t):
            return (.heading(level: level, t + EditorBlockContent.singleLine(tail)), EditorCaret(offset: t.utf16Count))
        case let .quote(t):
            return (.quote(t + tail), EditorCaret(offset: t.utf16Count))
        case var .list(items):
            let k = items.count - 1
            let at = items[k].text.utf16Count
            items[k].text += EditorBlockContent.singleLine(tail)
            return (.list(items), EditorCaret(line: k, offset: at))
        case .raw:
            return nil
        }
    }

    /// Where the caret sits at the end of a block.
    public static func endCaret(_ content: EditorBlockContent) -> EditorCaret {
        switch content {
        case let .list(items): EditorCaret(line: items.count - 1, offset: items.last?.text.utf16Count ?? 0)
        case let .raw(s): EditorCaret(offset: s.utf16.count)
        default: EditorCaret(offset: content.inline.utf16Count)
        }
    }

    // MARK: shortcuts

    /// Markdown typed at the start of a block (`# `, `- `, `1. `, `[ ] `,
    /// `> `), checked after each keystroke with the caret right after it.
    public static func shortcut(_ content: EditorBlockContent, caret: EditorCaret) -> (EditorBlockContent, EditorCaret)? {
        switch content {
        case let .paragraph(t):
            let plain = String(t.characters)
            let firstLine = plain.prefix { $0 != "\n" }
            guard caret.line == 0, caret.offset <= firstLine.utf16.count else { return nil }
            let typed = String(String(firstLine).utf16.prefix(caret.offset)) ?? ""
            guard let (kind, trigger) = trigger(typed), trigger.utf16.count == caret.offset else { return nil }
            let rest = t.split(utf16: trigger.utf16.count).1
            switch kind {
            case let .heading(level):
                return (.heading(level: level, EditorBlockContent.singleLine(rest)), .start)
            case .quote:
                return (.quote(rest), .start)
            case let .list(prefix):
                // each line of the paragraph becomes an item
                let lines = splitLines(rest)
                return (.list(lines.map { EditorListItem(prefix, $0) }), .start)
            }
        case var .list(items):
            // `[ ] ` at the start of an item makes it a task
            let k = caret.line
            guard items.indices.contains(k), items[k].prefix.checkbox == nil else { return nil }
            let plain = String(items[k].text.characters)
            for (box, checked) in [("[ ] ", false), ("[] ", false), ("[x] ", true)] where plain.hasPrefix(box) && caret.offset == box.utf16.count {
                items[k].prefix.checkbox = checked
                items[k].text = items[k].text.split(utf16: box.utf16.count).1
                return (.list(items), EditorCaret(line: k, offset: 0))
            }
            return nil
        default:
            return nil
        }
    }

    enum Trigger {
        case heading(Int)
        case quote
        case list(ListPrefix)
    }

    static func trigger(_ typed: String) -> (Trigger, String)? {
        if typed.hasSuffix(" "), typed.allSatisfy({ $0 == "#" || $0 == " " }) {
            let level = typed.prefix { $0 == "#" }.count
            if (1...6).contains(level), typed == String(repeating: "#", count: level) + " " { return (.heading(level), typed) }
        }
        switch typed {
        case "- ", "* ", "+ ": return (.list(ListPrefix(marker: typed.first!)), typed)
        case "> ": return (.quote, typed)
        case "[ ] ", "[] ": return (.list(.task), typed)
        case "[x] ": return (.list(ListPrefix(checkbox: true)), typed)
        case "- [ ] ", "* [ ] ": return (.list(ListPrefix(marker: typed.first!, checkbox: false)), typed)
        default: break
        }
        // `1. ` / `3) `
        let digits = typed.prefix { $0.isASCII && $0.isNumber }
        if (1...9).contains(digits.count), let n = Int(digits) {
            let rest = typed.dropFirst(digits.count)
            if rest == ". " || rest == ") " {
                return (.list(ListPrefix(ordered: true, marker: rest.first!, number: n)), typed)
            }
        }
        return nil
    }

    static func splitLines(_ t: AttributedString) -> [AttributedString] {
        var out: [AttributedString] = []
        var rest = t
        while let nl = rest.characters.firstIndex(of: "\n") {
            out.append(AttributedString(rest[rest.startIndex..<nl]))
            rest = AttributedString(rest[rest.characters.index(after: nl)..<rest.endIndex])
        }
        out.append(rest)
        return out
    }

    // MARK: lists

    /// Tab: item `caret.line` and its sub-items one level deeper (never
    /// more than one level below the item above).
    public static func indent(_ content: EditorBlockContent, at caret: EditorCaret) -> BlockCommandResult {
        guard case var .list(items) = content, items.indices.contains(caret.line), caret.line > 0 else { return .none }
        let k = caret.line
        guard items[k].prefix.indent <= items[k - 1].prefix.indent else { return .none }
        for i in subtree(items, k) { items[i].prefix.indent += 1 }
        return .update(.list(renumbered(items)), caret: caret)
    }

    /// Shift-Tab: item `caret.line` and its sub-items one level out.
    public static func outdent(_ content: EditorBlockContent, at caret: EditorCaret) -> BlockCommandResult {
        guard case var .list(items) = content, items.indices.contains(caret.line), items[caret.line].prefix.indent > 0 else { return .none }
        for i in subtree(items, caret.line) { items[i].prefix.indent -= 1 }
        return .update(.list(renumbered(items)), caret: caret)
    }

    /// `k` and the items nested under it.
    static func subtree(_ items: [EditorListItem], _ k: Int) -> Range<Int> {
        var end = k + 1
        while end < items.count, items[end].prefix.indent > items[k].prefix.indent { end += 1 }
        return k..<end
    }

    /// A list split off another starts at depth 0 and number 1.
    static func rebased(_ items: [EditorListItem]) -> [EditorListItem] {
        guard let base = items.map(\.prefix.indent).min() else { return items }
        var out = items
        for i in out.indices { out[i].prefix.indent -= base }
        if let first = out.first, first.prefix.ordered { out[0].prefix.number = 1 }
        return renumbered(out)
    }

    /// Ordered items numbered in sequence within each run (what the
    /// serialiser writes, mirrored so the markers on screen agree).
    public static func renumbered(_ items: [EditorListItem]) -> [EditorListItem] {
        var out = items
        var last: [Int: Int] = [:]
        for i in out.indices {
            let d = out[i].prefix.indent
            for deeper in last.keys where deeper > d { last[deeper] = nil }
            if out[i].prefix.ordered {
                if let n = last[d] { out[i].prefix.number = n + 1 }
                last[d] = out[i].prefix.number
            } else {
                last[d] = nil
            }
        }
        return out
    }

    /// The to-do toggle: a task item loses its box, anything else gains one.
    public static func toggleTodo(_ content: EditorBlockContent, at caret: EditorCaret) -> (EditorBlockContent, EditorCaret)? {
        switch content {
        case var .list(items):
            guard items.indices.contains(caret.line) else { return nil }
            items[caret.line].prefix.checkbox = items[caret.line].prefix.checkbox == nil ? false : nil
            return (.list(items), caret)
        case .raw:
            return nil
        default:
            return convert(content, to: .todo, caret: caret)
        }
    }

    /// Tick or untick the box on item `k`.
    public static func setChecked(_ content: EditorBlockContent, item k: Int, _ checked: Bool) -> EditorBlockContent {
        guard case var .list(items) = content, items.indices.contains(k), items[k].prefix.checkbox != nil else { return content }
        items[k].prefix.checkbox = checked
        return .list(items)
    }

    // MARK: kinds

    public static func kind(of content: EditorBlockContent, line: Int = 0) -> BlockKindChoice? {
        switch content {
        case .paragraph: return .paragraph
        case let .heading(level, _): return level == 1 ? .heading1 : level == 2 ? .heading2 : .heading3
        case .quote: return .quote
        case let .list(items):
            let p = items[max(0, min(line, items.count - 1))].prefix
            return p.checkbox != nil ? .todo : p.ordered ? .numbered : .bullet
        case .raw: return nil
        }
    }

    /// The block as another kind. Lists split into items by line; a list
    /// joins its items with line breaks. Raw blocks don't convert.
    public static func convert(_ content: EditorBlockContent, to choice: BlockKindChoice, caret: EditorCaret) -> (EditorBlockContent, EditorCaret)? {
        if content.isRaw { return nil }
        let inline = content.inline
        // the caret in the joined text
        var flat = caret.offset
        if case let .list(items) = content {
            flat = items.prefix(caret.line).reduce(0) { $0 + $1.text.utf16Count + 1 } + caret.offset
        }
        func listed(_ make: (Int, ListPrefix?) -> ListPrefix) -> (EditorBlockContent, EditorCaret) {
            if case let .list(items) = content {
                let out = items.enumerated().map { i, it in EditorListItem(make(i, it.prefix), it.text) }
                return (.list(renumbered(out)), caret)
            }
            let lines = splitLines(inline)
            // the caret's line and offset in it
            var line = 0, rem = flat
            for (i, l) in lines.enumerated() {
                if rem <= l.utf16Count { line = i; break }
                rem -= l.utf16Count + 1
                line = i + 1
            }
            let items = lines.enumerated().map { i, l in EditorListItem(make(i, nil), l) }
            return (.list(renumbered(items)), EditorCaret(line: min(line, items.count - 1), offset: max(0, rem)))
        }
        switch choice {
        case .paragraph: return (.paragraph(inline), EditorCaret(offset: flat))
        case .heading1: return (.heading(level: 1, EditorBlockContent.singleLine(inline)), EditorCaret(offset: flat))
        case .heading2: return (.heading(level: 2, EditorBlockContent.singleLine(inline)), EditorCaret(offset: flat))
        case .heading3: return (.heading(level: 3, EditorBlockContent.singleLine(inline)), EditorCaret(offset: flat))
        case .quote: return (.quote(inline), EditorCaret(offset: flat))
        case .bullet:
            return listed { _, p in ListPrefix(indent: p?.indent ?? 0, marker: p.flatMap { $0.ordered ? nil : $0.marker } ?? "-") }
        case .numbered:
            return listed { i, p in ListPrefix(indent: p?.indent ?? 0, ordered: true, number: i + 1) }
        case .todo:
            return listed { _, p in ListPrefix(indent: p?.indent ?? 0, ordered: p?.ordered ?? false, marker: p?.marker, number: p?.number ?? 1, checkbox: p?.checkbox ?? false) }
        }
    }
}
