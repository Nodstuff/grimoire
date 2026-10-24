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
        /// blocks under an inline change card: their text is the card's to show
        var covered: Set<BlockID> = []
        /// room left under a covered block's last line for its card
        var cardSpace: [BlockID: CGFloat] = [:]
    }

    static let indentStep: CGFloat = 22

    /// The display keys a restyle owns (cleared and set again).
    static let displayKeys: [NSAttributedString.Key] = [
        .font, .foregroundColor, .paragraphStyle, .backgroundColor, .strikethroughStyle,
        .underlineStyle, .underlineColor, .link, .attachment, .kern, .accessibilityTextHeadingLevel,
    ]

    static func font(_ style: UIFont.TextStyle, serif: Bool = false, bold: Bool = false, traits: UITraitCollection?) -> UIFont {
        var d = UIFontDescriptor.preferredFontDescriptor(withTextStyle: style, compatibleWith: traits)
        if serif, let s = d.withDesign(.serif) { d = s }
        if bold, let b = d.withSymbolicTraits(.traitBold) { d = b }
        let scale = traits?.docScale ?? 1
        return UIFont(descriptor: d, size: scale == 1 ? 0 : d.pointSize * scale)
    }

    /// A line's font before inline marks: Newsreader for text, headings
    /// 1-3 and quotes (Lapis: docs read in Newsreader); SF for level 4+.
    static func baseFont(_ style: Surface.LineStyle, traits: UITraitCollection?) -> UIFont {
        font(style, marks: [], traits: traits)
    }

    /// A line's font with its inline marks applied. Text and quotes read in
    /// Newsreader; headings are SF Pro bold (the Lapis doc artboards: the
    /// structure in the interface face, the words in the reading face).
    static func font(_ style: Surface.LineStyle, marks: InlineMarks, traits: UITraitCollection?) -> UIFont {
        var italic = false
        switch style {
        case .paragraph, .item, .raw: break
        case .quote: italic = true
        case let .heading(l):
            return font(headingFont(l, traits: traits), marks: marks)
        }
        let lapis = LapisTextStyle.docBody
        let base = lapis.serif.weight
        let factor = bodyFactor(traits)
        if marks.contains(.code) {
            let size = lapis.uiFont(traits: traits).pointSize * 0.85 * factor
            return UIFont.monospacedSystemFont(ofSize: size, weight: marks.contains(.bold) ? .semibold : .regular)
        }
        let weight: UIFont.Weight = marks.contains(.bold) ? .semibold : base
        if marks.contains(.italic) { italic.toggle() }
        let f = lapis.uiFont(traits: traits, weight: weight, italic: italic)
        return factor == 1 ? f : f.withSize(f.pointSize * factor)
    }

    /// The doc artboards read a step above the foundation's 18 pt body:
    /// 19 pt on iPhone, 20 pt on a regular-width iPad (the Mac scales by docScale).
    static func bodyFactor(_ traits: UITraitCollection?) -> CGFloat {
        #if targetEnvironment(macCatalyst)
        return 1
        #else
        switch traits?.userInterfaceIdiom {
        case .phone: return 19 / 18
        case .pad: return traits?.horizontalSizeClass == .regular ? 20 / 18 : 19 / 18
        default: return 1
        }
        #endif
    }

    /// A heading: SF Pro bold at the doc's heading sizes (scaled like the
    /// text: Dynamic Type, and the Mac's docScale).
    static func headingFont(_ level: Int, traits: UITraitCollection?) -> UIFont {
        let style: LapisTextStyle = level <= 1 ? .docHeading1 : level == 2 ? .docHeading2 : .docHeading3
        // the serif sizes read large in SF: a step down keeps the artboard's 21 / 19 pt
        let serifSize = style.uiFont(traits: traits).pointSize
        let size = level <= 3 ? serifSize * 0.88 : serifSize * 0.8
        var d = UIFont.systemFont(ofSize: size, weight: level <= 2 ? .bold : .semibold).fontDescriptor
        if traits?.legibilityWeight == .bold, let b = d.withSymbolicTraits(.traitBold) { d = b }
        return UIFont(descriptor: d, size: size)
    }

    /// Inline marks on a system font (headings past level 3).
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
        // Newsreader reads at ~1.55 leading (the web's prose)
        p.lineSpacing = 5
        p.baseWritingDirection = .natural
        switch style {
        case .paragraph:
            p.paragraphSpacing = 10
        case let .heading(l):
            p.paragraphSpacingBefore = l <= 2 ? 16 : 10
            p.paragraphSpacing = 6
            p.lineSpacing = 2
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
        let checked = style.prefix?.checkbox == true
        var out: [NSAttributedString.Key: Any] = [
            .font: font(style, marks: marks, traits: ctx.traits),
            .foregroundColor: checked ? UIColor.lapis.ink3 : style == .quote ? UIColor.lapis.ink2 : UIColor.lapis.ink,
        ]
        if a[Surface.marker] != nil {
            out[.font] = baseFont(style, traits: ctx.traits)
            // a checkbox's glyph is clipped out and its box drawn by
            // CheckboxLayoutFragment (one shape, ticked or not)
            out[.foregroundColor] = style.prefix?.checkbox != nil ? UIColor.clear : UIColor.lapis.ink3
            return out
        }
        if case .heading = style { out[.kern] = -0.3 }
        // inline code: a rounded, outlined pill drawn by SurfaceLayoutFragment
        if marks.contains(.strike) || checked { out[.strikethroughStyle] = NSUnderlineStyle.single.rawValue }
        let link = a[Surface.link] as? String
        let wiki = a[Surface.wiki] as? String
        if link != nil {
            out[.foregroundColor] = UIColor.lapis.accent
            out[.underlineStyle] = NSUnderlineStyle.single.rawValue
        }
        if wiki != nil {
            // the accent with a soft rule under it (the doc artboards)
            out[.foregroundColor] = UIColor.lapis.accent
            out[.underlineStyle] = NSUnderlineStyle.thick.rawValue
            out[.underlineColor] = UIColor.lapis[.accentRing]
        }
        // read-only text follows links on a tap; the editable surface
        // places the caret (⌘-click and the edit menu follow them)
        if !ctx.editable, let u = url(link: link, wiki: wiki) { out[.link] = u }
        return out
    }

    /// Restyle every line touching `range` (all of it when nil).
    static func restyle(_ s: NSMutableAttributedString, range: NSRange? = nil, ctx: Context) {
        guard s.length > 0 else { return }
        // only the lines touched (no whole-doc scan per keystroke), plus
        // the line after (a list's last item gets the block spacing)
        var ls = range.map { r in
            Surface.lines(s, in: NSRange(location: min(r.location, s.length - 1), length: min(r.length, s.length - min(r.location, s.length - 1))))
        } ?? Surface.lines(s)
        guard !ls.isEmpty else { return }
        // a list restyles whole: widen to its first and last items
        while let f = ls.first, f.style.isItem, f.range.location > 0,
              let prev = Surface.line(at: f.range.location - 1, in: s), prev.style.isItem, prev.id == f.id { ls.insert(prev, at: 0) }
        while let l = ls.last, l.style.isItem, l.end < s.length,
              let next = Surface.line(at: l.end, in: s), next.style.isItem, next.id == l.id { ls.append(next) }
        let after = ls.last.flatMap { $0.end < s.length ? Surface.line(at: $0.end, in: s) : nil }
        for i in ls.indices {
            let l = ls[i]
            let following = i + 1 < ls.count ? ls[i + 1] : after
            let lastItem = following.map { $0.id != l.id || !$0.style.isItem } ?? true
            var para = paragraphStyle(l.style, first: i == 0, last: lastItem)
            let covered = l.id.map { ctx.covered.contains($0) } ?? false
            // a covered block's last line leaves room for its card
            if covered, let id = l.id, let space = ctx.cardSpace[id], following.map({ $0.id != id }) ?? true {
                let p = para.mutableCopy() as! NSMutableParagraphStyle
                p.paragraphSpacing = max(p.paragraphSpacing, space)
                para = p
            }
            let tint = covered ? nil : l.id.flatMap { ctx.tints[$0] }
            for k in displayKeys { s.removeAttribute(k, range: l.range) }
            s.addAttribute(.paragraphStyle, value: para, range: l.range)
            s.enumerateAttributes(in: l.range) { a, run, _ in
                var d = display(a, style: l.style, ctx: ctx)
                if let tint, d[.backgroundColor] == nil { d[.backgroundColor] = tint }
                if covered {
                    // the card shows the change; the text under it stays out of sight
                    d[.foregroundColor] = UIColor.clear
                    d[.backgroundColor] = nil
                    d[.underlineStyle] = nil
                    d[.strikethroughStyle] = nil
                }
                s.addAttributes(d, range: run)
            }
            if l.style == .raw, l.contentRange.length == 1, let src = s.attribute(Surface.raw, at: l.range.location, effectiveRange: nil) as? String,
               let id = l.id, let att = ctx.attachment(id, src) {
                att.accessibilityLabel = AttachmentContent.label(src)
                s.addAttribute(.attachment, value: att, range: NSRange(location: l.range.location, length: 1))
            }
            // VoiceOver's headings rotor
            if case let .heading(n) = l.style { s.addAttribute(.accessibilityTextHeadingLevel, value: n, range: l.range) }
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
