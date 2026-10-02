import SwiftUI
import UIKit

/// The Mac's doc text size (View › Bigger Text / Smaller Text / Actual
/// Size, ⌘+ ⌘- ⌘0). A step on the Dynamic Type scale, applied to the doc
/// screen only, so reading and editing grow together and the sidebar keeps
/// its size. The iPhone follows the system's Text Size instead.
enum DocTextSize {
    static let key = "doc.textSize"
    static let steps: [DynamicTypeSize] = [.medium, .large, .xLarge, .xxLarge, .xxxLarge, .accessibility1, .accessibility2]
    /// Catalyst's default, `.large`
    static let actual = 1

    static func size(_ step: Int) -> DynamicTypeSize { steps[clamp(step)] }
    static func bigger(_ step: Int) -> Int { clamp(step + 1) }
    static func smaller(_ step: Int) -> Int { clamp(step - 1) }
    static func canGrow(_ step: Int) -> Bool { clamp(step) < steps.count - 1 }
    static func canShrink(_ step: Int) -> Bool { clamp(step) > 0 }

    private static func clamp(_ step: Int) -> Int { min(max(step, 0), steps.count - 1) }
}

extension EnvironmentValues {
    /// How much bigger than the standard size doc text is drawn (1 = as
    /// the system draws it). Set by the doc screen on the Mac, where
    /// Catalyst keeps SwiftUI's text styles at one size whatever
    /// `dynamicTypeSize` says; the iPhone keeps 1 and follows the system.
    @Entry var docScale: CGFloat = 1
}

extension DocTextSize {
    /// The scale for a step: iOS's body size at that Dynamic Type size over
    /// the standard one (17 pt at `.large`). A table, because Catalyst
    /// answers every UIFont/SwiftUI size query at `.large` whatever is asked.
    static let scales: [CGFloat] = [16, 17, 19, 21, 23, 28, 33].map { $0 / 17 }
    static func scale(_ step: Int) -> CGFloat { scales[min(max(step, 0), scales.count - 1)] }
    static func scale(for size: DynamicTypeSize) -> CGFloat {
        steps.firstIndex(of: size).map(scale) ?? 1
    }

    static func point(_ style: UIFont.TextStyle, _ size: DynamicTypeSize) -> CGFloat {
        UIFont.preferredFont(forTextStyle: style, compatibleWith: UITraitCollection(preferredContentSizeCategory: UIContentSizeCategory(size))).pointSize
    }
}

/// A doc's text style, grown by `docScale` when it isn't 1.
struct DocFont: ViewModifier {
    let style: Font.TextStyle
    var design: Font.Design = .default
    var weight: Font.Weight?
    @Environment(\.docScale) private var scale

    func body(content: Content) -> some View {
        if scale == 1 {
            content.font(.system(style, design: design, weight: weight))
        } else {
            let size = DocTextSize.point(style.uiKit, .large) * scale
            content.font(.system(size: size, weight: weight ?? (style == .headline ? .semibold : .regular), design: design))
        }
    }
}

extension View {
    func docFont(_ style: Font.TextStyle, design: Font.Design = .default, weight: Font.Weight? = nil) -> some View {
        modifier(DocFont(style: style, design: design, weight: weight))
    }
}

extension Font.TextStyle {
    var uiKit: UIFont.TextStyle {
        switch self {
        case .largeTitle: .largeTitle
        case .title: .title1
        case .title2: .title2
        case .title3: .title3
        case .headline: .headline
        case .subheadline: .subheadline
        case .callout: .callout
        case .footnote: .footnote
        case .caption: .caption1
        case .caption2: .caption2
        default: .body
        }
    }
}

/// The doc text scale for UIKit text (the editor), alongside the SwiftUI
/// `docScale`: 1 everywhere but the Mac's doc screen.
struct DocScaleTrait: UITraitDefinition {
    static let defaultValue: CGFloat = 1
}

extension UITraitCollection {
    var docScale: CGFloat { self[DocScaleTrait.self] }
}

extension UIMutableTraits {
    var docScale: CGFloat {
        get { self[DocScaleTrait.self] }
        set { self[DocScaleTrait.self] = newValue }
    }
}
