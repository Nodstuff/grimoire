import UIKit
import TaisceKit

/// The bridge between a block's `EditorBlockContent` and the attributed
/// text in its `UITextView`. Formatting lives in custom attributes (the
/// model); fonts and colours are derived from them (the look). List items
/// carry a visible marker ("•", "1.", "☐") tagged with its `ListPrefix`,
/// which the caret never enters and the model never reads as text.
enum EditorText {
    static let marks = NSAttributedString.Key("taisce.marks")
    static let link = NSAttributedString.Key("taisce.link")
    static let wiki = NSAttributedString.Key("taisce.wiki")
    static let marker = NSAttributedString.Key("taisce.marker")

    /// Which look a block's text gets.
    enum Kind: Hashable {
        case paragraph, heading(Int), quote, list, raw

        init(_ c: EditorBlockContent) {
            switch c {
            case .paragraph: self = .paragraph
            case let .heading(level, _): self = .heading(level)
            case .quote: self = .quote
            case .list: self = .list
            case .raw: self = .raw
            }
        }

        var accessibilityName: String {
            switch self {
            case .paragraph: "Paragraph"
            case let .heading(l): "Heading \(l)"
            case .quote: "Quote"
            case .list: "List"
            case .raw: "Source"
            }
        }
    }

    // MARK: look

    static let indentStep: CGFloat = 22

    static func baseFont(_ kind: Kind, traits: UITraitCollection?) -> UIFont {
        func styled(_ style: UIFont.TextStyle, serif: Bool, bold: Bool) -> UIFont {
            var d = UIFontDescriptor.preferredFontDescriptor(withTextStyle: style, compatibleWith: traits)
            if serif, let s = d.withDesign(.serif) { d = s }
            if bold, let b = d.withSymbolicTraits(.traitBold) { d = b }
            let scale = traits?.docScale ?? 1
            return UIFont(descriptor: d, size: scale == 1 ? 0 : d.pointSize * scale)
        }
        switch kind {
        case .paragraph, .list: return styled(.body, serif: false, bold: false)
        case let .heading(l):
            switch l {
            case 1: return styled(.title2, serif: true, bold: true)
            case 2: return styled(.title3, serif: true, bold: true)
            case 3: return styled(.headline, serif: true, bold: true)
            default: return styled(.subheadline, serif: false, bold: true)
            }
        case .quote: return styled(.body, serif: true, bold: false)
        case .raw:
            let size = UIFont.preferredFont(forTextStyle: .footnote, compatibleWith: traits).pointSize * (traits?.docScale ?? 1)
            return UIFont.monospacedSystemFont(ofSize: size, weight: .regular)
        }
    }

    static func font(_ base: UIFont, marks: InlineMarks) -> UIFont {
        if marks.contains(.code) {
            return UIFont.monospacedSystemFont(ofSize: base.pointSize * 0.9, weight: marks.contains(.bold) ? .semibold : .regular)
        }
        var traits = base.fontDescriptor.symbolicTraits
        if marks.contains(.bold) { traits.insert(.traitBold) }
        if marks.contains(.italic) { traits.insert(.traitItalic) }
        guard let d = base.fontDescriptor.withSymbolicTraits(traits) else { return base }
        return UIFont(descriptor: d, size: 0)
    }

    static func paragraphStyle(_ kind: Kind, prefix: ListPrefix? = nil) -> NSParagraphStyle {
        let p = NSMutableParagraphStyle()
        p.lineSpacing = 3
        p.paragraphSpacing = 2
        switch kind {
        case .quote:
            p.firstLineHeadIndent = 14
            p.headIndent = 14
        case .list:
            let lead = CGFloat(prefix?.indent ?? 0) * indentStep
            let width: CGFloat = (prefix?.ordered ?? false) ? 30 : 24
            p.firstLineHeadIndent = lead
            p.headIndent = lead + width
            p.tabStops = [NSTextTab(textAlignment: .natural, location: lead + width)]
            p.defaultTabInterval = width
        default:
            break
        }
        // RTL text lays out right to left inside the same block
        p.baseWritingDirection = .natural
        return p
    }

    /// Display attributes for text with these model attributes.
    static func look(_ kind: Kind, marks: InlineMarks, link: String?, wiki: String?, checked: Bool, traits: UITraitCollection?, prefix: ListPrefix? = nil) -> [NSAttributedString.Key: Any] {
        var a: [NSAttributedString.Key: Any] = [
            .font: font(baseFont(kind, traits: traits), marks: kind == .raw ? [] : marks),
            .foregroundColor: kind == .quote || checked ? UIColor(Theme.secondary) : UIColor(Theme.text),
            .paragraphStyle: paragraphStyle(kind, prefix: prefix),
        ]
        if kind == .raw { return a }
        if marks.contains(.code) { a[.backgroundColor] = UIColor(Theme.surface2) }
        if marks.contains(.strike) || checked { a[.strikethroughStyle] = NSUnderlineStyle.single.rawValue }
        if link != nil {
            a[.foregroundColor] = UIColor(Theme.accentActive)
            a[.underlineStyle] = NSUnderlineStyle.single.rawValue
        }
        if wiki != nil {
            a[.foregroundColor] = UIColor(Theme.accentActive)
            a[.backgroundColor] = UIColor(Theme.accent).withAlphaComponent(0.14)
        }
        return a
    }

    // MARK: content → text

    static func markerText(_ p: ListPrefix) -> String {
        if let box = p.checkbox { return (box ? "☑" : "☐") + "\t" }
        if p.ordered { return "\(p.number)\(p.marker)\t" }
        return (p.indent % 2 == 0 ? "•" : "◦") + "\t"
    }

    static func encode(_ p: ListPrefix) -> String {
        "\(p.indent)|\(p.ordered ? 1 : 0)|\(p.marker)|\(p.number)|\(p.checkbox.map { $0 ? "1" : "0" } ?? "-")"
    }

    static func decode(_ s: String) -> ListPrefix? {
        let f = s.split(separator: "|", omittingEmptySubsequences: false)
        guard f.count == 5, let indent = Int(f[0]), let n = Int(f[3]), let m = f[2].first else { return nil }
        return ListPrefix(indent: indent, ordered: f[1] == "1", marker: m, number: n, checkbox: f[4] == "-" ? nil : f[4] == "1")
    }

    static func inline(_ text: AttributedString, kind: Kind, checked: Bool = false, prefix: ListPrefix? = nil, traits: UITraitCollection?) -> NSMutableAttributedString {
        let out = NSMutableAttributedString()
        for r in InlineCodec.runs(text) {
            var a = look(kind, marks: r.marks, link: r.link, wiki: r.wiki, checked: checked, traits: traits, prefix: prefix)
            if !r.marks.isEmpty { a[marks] = r.marks.rawValue }
            if let l = r.link { a[link] = l }
            if let w = r.wiki { a[wiki] = w }
            out.append(NSAttributedString(string: r.text, attributes: a))
        }
        return out
    }

    static func attributed(_ content: EditorBlockContent, traits: UITraitCollection?) -> NSAttributedString {
        let kind = Kind(content)
        switch content {
        case let .paragraph(t), let .heading(_, t), let .quote(t):
            return inline(t, kind: kind, traits: traits)
        case let .list(items):
            let out = NSMutableAttributedString()
            for (i, item) in items.enumerated() {
                if i > 0 { out.append(NSAttributedString(string: "\n", attributes: look(.list, marks: [], link: nil, wiki: nil, checked: false, traits: traits, prefix: item.prefix))) }
                var m = look(.list, marks: [], link: nil, wiki: nil, checked: false, traits: traits, prefix: item.prefix)
                m[.foregroundColor] = UIColor(item.prefix.checkbox == true ? Theme.green : Theme.secondary)
                m[marker] = encode(item.prefix)
                out.append(NSAttributedString(string: markerText(item.prefix), attributes: m))
                out.append(inline(item.text, kind: .list, checked: item.prefix.checkbox == true, prefix: item.prefix, traits: traits))
            }
            return out
        case let .raw(s):
            return NSAttributedString(string: s, attributes: look(.raw, marks: [], link: nil, wiki: nil, checked: false, traits: traits))
        }
    }

    // MARK: text → content

    /// One line of a list block in the text view.
    struct Line {
        var range: NSRange
        /// the marker's length at the line's start (0 when it lost it)
        var markerLength: Int
        var prefix: ListPrefix?
        var textRange: NSRange { NSRange(location: range.location + markerLength, length: range.length - markerLength) }
    }

    static func lines(_ s: NSAttributedString) -> [Line] {
        let ns = s.string as NSString
        var out: [Line] = []
        var start = 0
        while true {
            let nl = ns.range(of: "\n", options: [], range: NSRange(location: start, length: ns.length - start))
            let end = nl.location == NSNotFound ? ns.length : nl.location
            let r = NSRange(location: start, length: end - start)
            var len = 0
            var prefix: ListPrefix?
            if r.length > 0, let v = s.attribute(marker, at: r.location, longestEffectiveRange: nil, in: r) as? String {
                var eff = NSRange()
                _ = s.attribute(marker, at: r.location, longestEffectiveRange: &eff, in: r)
                len = eff.length
                prefix = decode(v)
            }
            out.append(Line(range: r, markerLength: len, prefix: prefix))
            if nl.location == NSNotFound { break }
            start = nl.location + 1
        }
        return out
    }

    static func runs(_ s: NSAttributedString, in range: NSRange) -> AttributedString {
        var runs: [InlineRun] = []
        guard range.length > 0 else { return AttributedString() }
        s.enumerateAttributes(in: range) { a, r, _ in
            let text = (s.string as NSString).substring(with: r)
            runs.append(InlineRun(
                text,
                marks: InlineMarks(rawValue: (a[marks] as? Int) ?? 0),
                link: a[link] as? String,
                wiki: a[wiki] as? String
            ))
        }
        return InlineCodec.build(runs)
    }

    /// The model for what the text view holds now, shaped like `template`.
    static func content(from s: NSAttributedString, like template: EditorBlockContent) -> EditorBlockContent {
        let all = NSRange(location: 0, length: s.length)
        switch template {
        case .paragraph: return .paragraph(runs(s, in: all))
        case let .heading(level, _): return .heading(level: level, runs(s, in: all))
        case .quote: return .quote(runs(s, in: all))
        case .raw: return .raw(s.string)
        case let .list(old):
            var items: [EditorListItem] = []
            for line in lines(s) {
                // a line that lost its marker (a paste, a deletion) continues the one above
                let prefix = line.prefix ?? items.last?.prefix.continued ?? old.first?.prefix ?? .bullet
                items.append(EditorListItem(prefix, runs(s, in: line.textRange)))
            }
            return .list(EditorCommands.renumbered(items))
        }
    }

    // MARK: carets

    static func caret(at location: Int, in s: NSAttributedString, kind: Kind) -> EditorCaret {
        guard kind == .list else { return EditorCaret(offset: location) }
        let ls = lines(s)
        for (i, l) in ls.enumerated() where location <= l.range.location + l.range.length {
            return EditorCaret(line: i, offset: max(0, location - l.range.location - l.markerLength))
        }
        let last = ls.count - 1
        return EditorCaret(line: last, offset: ls[last].textRange.length)
    }

    static func location(of caret: EditorCaret, in s: NSAttributedString, kind: Kind) -> Int {
        guard kind == .list else { return min(max(0, caret.offset), s.length) }
        let ls = lines(s)
        guard !ls.isEmpty else { return 0 }
        let l = ls[max(0, min(caret.line, ls.count - 1))]
        return l.range.location + l.markerLength + min(max(0, caret.offset), l.textRange.length)
    }

    /// The text before the caret on its line, for `[[` completion.
    static func textBeforeCaret(_ location: Int, in s: NSAttributedString, kind: Kind) -> String {
        let ns = s.string as NSString
        let loc = min(location, ns.length)
        if kind == .list {
            let c = caret(at: loc, in: s, kind: kind)
            let l = lines(s)[c.line]
            return ns.substring(with: NSRange(location: l.textRange.location, length: max(0, loc - l.textRange.location)))
        }
        return ns.substring(to: loc)
    }
}
