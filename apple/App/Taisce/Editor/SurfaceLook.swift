import UIKit
import TaisceKit

/// The look of the one-surface editor: fonts, colours, spacing and
/// attachments drawn from `Surface`'s hidden model attributes. The model
/// attributes are never touched here; only display ones are set.
enum SurfaceLook {
    /// What a restyle needs beyond the text.
    struct Context {
        var traits: UITraitCollection?
        var editable = true
        /// blocks to tint: flagged / parked / refused / in conflict
        var tints: [BlockID: UIColor] = [:]
        /// the view for a non-text block
        var attachment: (BlockID, String) -> NSTextAttachment? = { _, _ in nil }
    }

    static let indentStep: CGFloat = 22

    /// The display keys a restyle owns (cleared and set again).
    static let displayKeys: [NSAttributedString.Key] = [
        .font, .foregroundColor, .paragraphStyle, .backgroundColor, .strikethroughStyle,
        .underlineStyle, .underlineColor, .link, .attachment, .kern,
    ]

    static func font(_ style: UIFont.TextStyle, serif: Bool = false, bold: Bool = false, traits: UITraitCollection?) -> UIFont {
        var d = UIFontDescriptor.preferredFontDescriptor(withTextStyle: style, compatibleWith: traits)
        if serif, let s = d.withDesign(.serif) { d = s }
        if bold, let b = d.withSymbolicTraits(.traitBold) { d = b }
        let scale = traits?.docScale ?? 1
        return UIFont(descriptor: d, size: scale == 1 ? 0 : d.pointSize * scale)
    }

    static func baseFont(_ style: Surface.LineStyle, traits: UITraitCollection?) -> UIFont {
        switch style {
        case .paragraph, .item, .raw: return font(.body, traits: traits)
        case let .heading(l):
            switch l {
            case 1: return font(.title2, serif: true, bold: true, traits: traits)
            case 2: return font(.title3, serif: true, bold: true, traits: traits)
            case 3: return font(.headline, serif: true, bold: true, traits: traits)
            default: return font(.subheadline, bold: true, traits: traits)
            }
        case .quote: return font(.body, serif: true, traits: traits)
        }
    }

    static func font(_ base: UIFont, marks: InlineMarks) -> UIFont {
        if marks.contains(.code) {
            return UIFont.monospacedSystemFont(ofSize: base.pointSize * 0.9, weight: marks.contains(.bold) ? .semibold : .regular)
        }
        var t = base.fontDescriptor.symbolicTraits
        if marks.contains(.bold) { t.insert(.traitBold) }
        if marks.contains(.italic) { t.insert(.traitItalic) }
        guard t != base.fontDescriptor.symbolicTraits, let d = base.fontDescriptor.withSymbolicTraits(t) else { return base }
        return UIFont(descriptor: d, size: 0)
    }

    static func paragraphStyle(_ style: Surface.LineStyle, first: Bool, last: Bool) -> NSParagraphStyle {
        let p = NSMutableParagraphStyle()
        p.lineSpacing = 3
        p.baseWritingDirection = .natural
        switch style {
        case .paragraph:
            p.paragraphSpacing = 10
        case let .heading(l):
            p.paragraphSpacingBefore = l <= 2 ? 14 : 8
            p.paragraphSpacing = 6
        case .quote:
            p.firstLineHeadIndent = 14
            p.headIndent = 14
            p.paragraphSpacing = 10
        case let .item(prefix):
            let lead = CGFloat(prefix.indent) * indentStep
            let width: CGFloat = prefix.ordered ? 30 : 24
            p.firstLineHeadIndent = lead
            p.headIndent = lead + width
            p.tabStops = [NSTextTab(textAlignment: .natural, location: lead + width)]
            p.defaultTabInterval = width
            p.paragraphSpacing = last ? 10 : 4
        case .raw:
            p.paragraphSpacingBefore = 4
            p.paragraphSpacing = 12
        }
        return p
    }

    /// The URL a link or wikilink run opens.
    static func url(link: String?, wiki: String?) -> URL? {
        if let wiki {
            let target = String(wiki.split(separator: "|", maxSplits: 1).first ?? Substring(wiki))
            let enc = target.addingPercentEncoding(withAllowedCharacters: .alphanumerics) ?? target
            return URL(string: "\(InlineMarkdown.wikiScheme)://wiki/\(enc)")
        }
        return link.flatMap(URL.init(string:))
    }

    /// Display attributes for a run with these model attributes.
    static func display(_ a: [NSAttributedString.Key: Any], style: Surface.LineStyle, ctx: Context) -> [NSAttributedString.Key: Any] {
        let marks = InlineMarks(rawValue: (a[Surface.marks] as? Int) ?? 0)
        let base = baseFont(style, traits: ctx.traits)
        let checked = style.prefix?.checkbox == true
        var out: [NSAttributedString.Key: Any] = [
            .font: font(base, marks: marks),
            .foregroundColor: style == .quote || checked ? UIColor(Theme.secondary) : UIColor(Theme.text),
        ]
        if a[Surface.marker] != nil {
            out[.font] = base
            out[.foregroundColor] = UIColor(style.prefix?.checkbox == true ? Theme.green : Theme.secondary)
            return out
        }
        if marks.contains(.code) { out[.backgroundColor] = UIColor(Theme.surface2) }
        if marks.contains(.strike) || checked { out[.strikethroughStyle] = NSUnderlineStyle.single.rawValue }
        let link = a[Surface.link] as? String
        let wiki = a[Surface.wiki] as? String
        if link != nil {
            out[.foregroundColor] = UIColor(Theme.accentActive)
            out[.underlineStyle] = NSUnderlineStyle.single.rawValue
        }
        if wiki != nil {
            out[.foregroundColor] = UIColor(Theme.accentActive)
            out[.backgroundColor] = UIColor(Theme.accent).withAlphaComponent(0.14)
        }
        // read-only text follows links on a tap; the editable surface
        // places the caret (⌘-click and the edit menu follow them)
        if !ctx.editable, let u = url(link: link, wiki: wiki) { out[.link] = u }
        return out
    }

    /// Restyle every line touching `range` (all of it when nil).
    static func restyle(_ s: NSMutableAttributedString, range: NSRange? = nil, ctx: Context) {
        let ls = Surface.lines(s)
        guard !ls.isEmpty else { return }
        let r = range ?? NSRange(location: 0, length: s.length)
        let firstLine = Surface.lineIndex(ls, at: max(0, min(r.location, s.length - 1)))
        let lastLine = Surface.lineIndex(ls, at: max(0, min(r.location + r.length, s.length - 1)))
        // a list's last item gets the block spacing: widen to whole blocks
        var lo = firstLine, hi = lastLine
        while lo > 0, ls[lo - 1].style.isItem, ls[lo - 1].id == ls[lo].id { lo -= 1 }
        while hi < ls.count - 1, ls[hi + 1].style.isItem, ls[hi + 1].id == ls[hi].id { hi += 1 }
        for i in lo...hi {
            let l = ls[i]
            let lastItem = i == ls.count - 1 || ls[i + 1].id != l.id || !ls[i + 1].style.isItem
            let para = paragraphStyle(l.style, first: i == 0, last: lastItem)
            let tint = l.id.flatMap { ctx.tints[$0] }
            for k in displayKeys { s.removeAttribute(k, range: l.range) }
            s.addAttribute(.paragraphStyle, value: para, range: l.range)
            s.enumerateAttributes(in: l.range) { a, run, _ in
                var d = display(a, style: l.style, ctx: ctx)
                if let tint, d[.backgroundColor] == nil { d[.backgroundColor] = tint }
                s.addAttributes(d, range: run)
            }
            if l.style == .raw, l.contentRange.length == 1, let src = s.attribute(Surface.raw, at: l.range.location, effectiveRange: nil) as? String,
               let id = l.id, let att = ctx.attachment(id, src) {
                s.addAttribute(.attachment, value: att, range: NSRange(location: l.range.location, length: 1))
            }
        }
    }

    /// What typing inserts next at a location: the line's model attributes
    /// and marks (or `pending` ones from the bar), in their look.
    static func typing(_ s: NSAttributedString, at location: Int, pending: InlineMarks?, ctx: Context) -> [NSAttributedString.Key: Any] {
        var model = Surface.typingAttributes(s, at: location)
        if let pending {
            if pending.isEmpty { model[Surface.marks] = nil } else { model[Surface.marks] = pending.rawValue }
        }
        let style = (model[Surface.line] as? String).map(Surface.LineStyle.init(encoded:)) ?? .paragraph
        var out = model
        for (k, v) in display(model, style: style, ctx: ctx) where k != .link { out[k] = v }
        out[.paragraphStyle] = paragraphStyle(style, first: false, last: true)
        return out
    }
}
