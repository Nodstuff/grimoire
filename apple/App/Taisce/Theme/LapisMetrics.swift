import SwiftUI

/// Lapis spacing, radii, layout, shadows and motion (`tokens.css`).
extension Lapis {
    /// The 4 pt grid.
    enum Space {
        static let s1: CGFloat = 4
        static let s2: CGFloat = 8
        static let s3: CGFloat = 12
        static let s4: CGFloat = 16
        static let s5: CGFloat = 20
        static let s6: CGFloat = 24
        static let s7: CGFloat = 28
        static let s8: CGFloat = 32
        static let s10: CGFloat = 40
        static let s14: CGFloat = 56
    }

    enum Radius {
        /// keys
        static let xs: CGFloat = 5
        /// tags, the agent avatar
        static let sm: CGFloat = 6
        /// rows, small buttons, icon buttons
        static let md: CGFloat = 8
        static let button: CGFloat = 9
        static let field: CGFloat = 10
        /// cards
        static let card: CGFloat = 14
        /// sheets, popovers, dialogs
        static let xl: CGFloat = 18
        static let tab: CGFloat = 22
    }

    enum Layout {
        /// screen side padding on iPhone (the web's 16 px phone gutter, a
        /// touch wider for the larger iPhone corner radius)
        static let gutter: CGFloat = 20
        /// the measure of a doc's text
        static let readingWidth: CGFloat = 680
        /// the smallest thing a finger can hit (HIG)
        static let minTarget: CGFloat = 44
        /// a visible button's height inside its 44 pt target
        static let buttonHeight: CGFloat = 36
        static let smallButtonHeight: CGFloat = 30
    }

    enum Motion {
        /// `--ease`: cubic-bezier(0.2, 0.7, 0.2, 1)
        static func ease(_ duration: Double = 0.18) -> Animation { .timingCurve(0.2, 0.7, 0.2, 1, duration: duration) }
        /// `--dur-fast`
        static let fast = ease(0.12)
        /// `--dur`
        static let standard = ease(0.18)
        /// a spring for things that arrive (toasts, cards)
        static let arrive = Animation.spring(response: 0.38, dampingFraction: 0.86)
    }
}

/// Lapis shadows. Light: a soft two-layer lift; dark: a deeper single one
/// (the colour itself is a token, so each appearance gets its own).
enum LapisShadow {
    case none
    /// cards
    case card
    /// menus, popovers, toasts
    case pop
    /// a floating action button
    case fab
}

extension View {
    func lapisShadow(_ kind: LapisShadow = .card) -> some View {
        modifier(LapisShadowModifier(kind: kind))
    }
}

private struct LapisShadowModifier: ViewModifier {
    let kind: LapisShadow
    @Environment(\.colorScheme) private var scheme

    func body(content: Content) -> some View {
        switch kind {
        case .none:
            content
        case .card:
            content
                .shadow(color: Color.lapis.shadow.opacity(scheme == .dark ? 1 : 0.7), radius: scheme == .dark ? 15 : 12, y: scheme == .dark ? 10 : 8)
        case .pop:
            content.shadow(color: Color.lapis.shadowPop, radius: 20, y: 16)
        case .fab:
            content.shadow(color: Color.lapis.shadowPop.opacity(1.3), radius: 14, y: 12)
        }
    }
}
