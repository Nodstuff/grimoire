import SwiftUI
import UIKit

/// Lapis type. Docs, titles and covers read in **Newsreader** (Production
/// Type, OFL; bundled as two variable fonts, `Resources/Fonts`); the
/// interface is **SF Pro** in the system's semantic styles; agent names and
/// code are **SF Mono**. (The web uses Geist for its interface; on Apple
/// platforms the system face is the native choice.)
///
/// Every Newsreader style scales: with Dynamic Type on iPhone and iPad
/// (`UIFontMetrics` for the style it is relative to), and with the doc
/// screen's `docScale` on the Mac, where Catalyst answers every text style
/// at `.large` (see `DocTextSize`).
enum Newsreader {
    /// PostScript names of the variable fonts' default instances.
    static let roman = "Newsreader16pt-Regular"
    static let italic = "Newsreader16pt-Italic"

    /// false when the fonts aren't in the bundle (a test host built without
    /// resources): everything falls back to the system serif.
    static let isAvailable: Bool = UIFont(name: roman, size: 12) != nil

    private static let wght = 0x7767_6874 // 'wght'
    private static let opsz = 0x6F70_737A // 'opsz'

    /// Newsreader at a point size. `opsz` follows the size (6…72), as the
    /// type designer intends: tighter, finer letters for titles, sturdier
    /// ones for text.
    static func uiFont(size: CGFloat, weight: UIFont.Weight = .regular, italic: Bool = false) -> UIFont {
        guard isAvailable else {
            var d = UIFont.systemFont(ofSize: size, weight: weight).fontDescriptor
            if let s = d.withDesign(.serif) { d = s }
            if italic, let i = d.withSymbolicTraits(.traitItalic) { d = i }
            return UIFont(descriptor: d, size: size)
        }
        let variation: [Int: CGFloat] = [wght: axisWeight(weight), opsz: min(max(size, 6), 72)]
        let d = UIFontDescriptor(fontAttributes: [
            .name: italic ? Self.italic : roman,
            UIFontDescriptor.AttributeName(rawValue: kCTFontVariationAttribute as String): variation,
        ])
        return UIFont(descriptor: d, size: size)
    }

    /// UIKit's weight → the font's `wght` axis (200…800).
    static func axisWeight(_ w: UIFont.Weight) -> CGFloat {
        let steps: [(UIFont.Weight, CGFloat)] = [(.light, 200), (.regular, 300), (.medium, 400), (.semibold, 500), (.bold, 600), (.heavy, 700)]
        return steps.first { w.rawValue < $0.0.rawValue }?.1 ?? 800
    }

    /// One step heavier, for Bold Text (accessibility).
    static func bolder(_ w: UIFont.Weight) -> UIFont.Weight {
        let steps: [UIFont.Weight] = [.regular, .medium, .semibold, .bold, .heavy]
        return steps.first { w.rawValue < $0.rawValue } ?? .heavy
    }
}

/// The Lapis type scale. Serif styles are Newsreader; the rest are the
/// system's own semantic styles, listed here so screens name a role, not a
/// size.
enum LapisTextStyle: Hashable, Sendable, CaseIterable {
    /// Today's greeting, a big page title (42 on the web)
    case display
    /// a screen title: Library, To-dos (32 on the web's phone)
    case screenTitle
    /// a doc's title on its page (38 on the web)
    case docTitle
    /// headings inside a doc
    case docHeading1, docHeading2, docHeading3
    /// a doc's text (18 on the web, 1.55 leading)
    case docBody
    /// a doc title on a card, a search result, a child row
    case cardTitle
    /// a book's title on its cover and in lists
    case coverTitle

    // system styles, for completeness of the scale
    /// the interface's body (SF Pro)
    case ui
    /// secondary interface lines
    case uiSecondary
    /// meta: counts, crumbs, timestamps
    case meta
    /// an uppercase section label (DUE, PINNED)
    case label
    /// agent names, code, keys (SF Mono)
    case mono

    var isSerif: Bool {
        switch self {
        case .display, .screenTitle, .docTitle, .docHeading1, .docHeading2, .docHeading3, .docBody, .cardTitle, .coverTitle: true
        default: false
        }
    }

    /// Newsreader's size at the standard Dynamic Type size, its weight, and
    /// the system style it scales with.
    var serif: (size: CGFloat, weight: UIFont.Weight, relativeTo: UIFont.TextStyle) {
        switch self {
        case .display: (40, .medium, .largeTitle)
        case .screenTitle: (34, .medium, .largeTitle)
        case .docTitle: (34, .medium, .largeTitle)
        case .docHeading1: (27, .semibold, .title1)
        case .docHeading2: (23, .semibold, .title2)
        case .docHeading3: (20, .semibold, .title3)
        case .docBody: (18, .regular, .body)
        case .cardTitle: (17, .semibold, .headline)
        case .coverTitle: (17, .semibold, .headline)
        default: (17, .regular, .body)
        }
    }

    /// Tracking at the standard size (titles sit a touch tighter, labels wider).
    var tracking: CGFloat {
        switch self {
        case .display: -0.8
        case .screenTitle, .docTitle: -0.5
        case .docHeading1: -0.3
        case .label: 1.2
        default: 0
        }
    }

    /// Extra line spacing for reading text (1.55 leading on the web).
    var lineSpacing: CGFloat {
        switch self {
        case .docBody: 5
        case .display, .screenTitle, .docTitle: 0
        default: 0
        }
    }

    /// The system font for the non-serif roles.
    var systemFont: Font {
        switch self {
        case .ui: .body
        case .uiSecondary: .subheadline
        case .meta: .caption
        case .label: .caption.weight(.semibold)
        case .mono: .system(.footnote, design: .monospaced)
        default: .body
        }
    }
}

extension LapisTextStyle {
    /// The UIKit font for this style at a trait collection (the editor):
    /// Dynamic Type from `traits`, times the Mac's `docScale`.
    func uiFont(traits: UITraitCollection?, weight: UIFont.Weight? = nil, italic: Bool = false) -> UIFont {
        let s = serif
        let metrics = UIFontMetrics(forTextStyle: s.relativeTo)
        var size = metrics.scaledValue(for: s.size, compatibleWith: traits)
        let scale = traits?.docScale ?? 1
        if scale != 1 { size *= scale }
        var w = weight ?? s.weight
        if traits?.legibilityWeight == .bold { w = Newsreader.bolder(w) }
        return Newsreader.uiFont(size: size, weight: w, italic: italic)
    }
}

/// Applies a Lapis text style: Newsreader, scaled by Dynamic Type (iOS) or
/// `docScale` (the Mac's doc screen), with its tracking and leading; or
/// the system style for the interface roles.
struct LapisFont: ViewModifier {
    let style: LapisTextStyle
    var weight: Font.Weight?
    var italic = false
    @Environment(\.dynamicTypeSize) private var dynamicType
    @Environment(\.docScale) private var docScale
    @Environment(\.legibilityWeight) private var legibility

    func body(content: Content) -> some View {
        if style.isSerif {
            let font = uiFont
            content
                .font(Font(font))
                .tracking(style.tracking * font.pointSize / style.serif.size)
                .lineSpacing(style.lineSpacing * font.pointSize / style.serif.size)
        } else {
            content
                .font(weight.map { style.systemFont.weight($0) } ?? style.systemFont)
                .tracking(style.tracking)
        }
    }

    var uiFont: UIFont {
        let traits = UITraitCollection { t in
            t.preferredContentSizeCategory = UIContentSizeCategory(dynamicType)
            t.docScale = docScale
            t.legibilityWeight = legibility == .bold ? .bold : .regular
        }
        return style.uiFont(traits: traits, weight: weight?.uiKit, italic: italic)
    }
}

extension View {
    /// `Text("Library").lapisFont(.screenTitle)`; Newsreader styles scale
    /// with Dynamic Type and the Mac's doc text size.
    func lapisFont(_ style: LapisTextStyle, weight: Font.Weight? = nil, italic: Bool = false) -> some View {
        modifier(LapisFont(style: style, weight: weight, italic: italic))
    }
}

extension Font {
    /// Newsreader at a fixed size (artwork such as covers, where the
    /// layout, not Dynamic Type, sets the size).
    static func newsreader(_ size: CGFloat, weight: UIFont.Weight = .regular, italic: Bool = false) -> Font {
        Font(Newsreader.uiFont(size: size, weight: weight, italic: italic))
    }

    /// SF Mono in a text style (agent names, code).
    static func lapisMono(_ style: Font.TextStyle = .footnote, weight: Font.Weight = .regular) -> Font {
        .system(style, design: .monospaced, weight: weight)
    }
}

extension Font.Weight {
    var uiKit: UIFont.Weight {
        switch self {
        case .ultraLight: .ultraLight
        case .thin: .thin
        case .light: .light
        case .medium: .medium
        case .semibold: .semibold
        case .bold: .bold
        case .heavy: .heavy
        case .black: .black
        default: .regular
        }
    }
}
