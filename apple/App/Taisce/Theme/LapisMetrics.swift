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
        /// a sidebar destination row: a finger's 44 on iPad, a pointer's 30 on
        /// the Mac (Mac-Main's sidebar)
        static var sidebarRow: CGFloat {
            #if targetEnvironment(macCatalyst)
            30
            #else
            minTarget
            #endif
        }
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

/// A control drawn smaller than a finger (a 28 pt checkbox, the pencil on
/// a diagram) still takes a 44 pt tap: its hit shape reaches past what is
/// drawn, and nothing drawn moves or grows (the layout box stays the drawn
/// size). Where neighbours sit closer than 44 pt (list checkboxes, 8 pt
/// apart) the reach is capped so two targets never overlap.
struct TapArea: Equatable {
    /// what is drawn
    var drawn: CGSize
    /// the hit area wanted around it
    var target: CGSize
    /// at most this far past the trailing side (a neighbour sits there)
    var maxTrailing: CGFloat? = nil

    /// how far the hit shape reaches past each side
    var insets: (leading: CGFloat, trailing: CGFloat, v: CGFloat) {
        let h = max(0, (target.width - drawn.width) / 2)
        return (h, min(h, maxTrailing ?? h), max(0, (target.height - drawn.height) / 2))
    }

    /// the gap between a list checkbox and its item's text (ListBlock's
    /// HStack spacing), where a nested list's checkboxes also start
    static let checkboxGap: CGFloat = 10

    /// a doc checkbox: 28 pt in a list whose rows are 8 pt apart, so 44 wide
    /// and as tall as the row pitch (28 + 8) without reaching a neighbour
    static let checkbox = TapArea(drawn: CGSize(width: 28, height: 28), target: CGSize(width: Lapis.Layout.minTarget, height: 36))
    /// the checkbox of an item holding a nested checklist: the nested
    /// boxes start `checkboxGap` to its right, close enough below that the
    /// two reaches meet, and theirs reaches back into the gap; this one
    /// reaches only what is left of it, so the two never overlap
    static let checkboxOverNested = TapArea(drawn: checkbox.drawn, target: checkbox.target,
                                            maxTrailing: checkboxGap - checkbox.insets.leading)
    /// the pencil on a block shown as an attachment (a caption glyph in 6 pt of padding)
    static let editSource = TapArea(drawn: CGSize(width: 26, height: 26), target: CGSize(width: Lapis.Layout.minTarget, height: Lapis.Layout.minTarget))
    /// the sidebar's New folder (a 30 pt square)
    static let newFolder = TapArea(drawn: CGSize(width: 30, height: 30), target: CGSize(width: Lapis.Layout.minTarget, height: Lapis.Layout.minTarget))

    static func == (a: TapArea, b: TapArea) -> Bool {
        a.drawn == b.drawn && a.target == b.target && a.maxTrailing == b.maxTrailing
    }
}

extension View {
    /// Hit-testable across `area.target` around the view; drawn and laid
    /// out as before.
    func tapArea(_ area: TapArea) -> some View {
        let (l, t, v) = area.insets
        return padding(.leading, l).padding(.trailing, t).padding(.vertical, v)
            .contentShape(.rect)
            .padding(.leading, -l).padding(.trailing, -t).padding(.vertical, -v)
    }
}
