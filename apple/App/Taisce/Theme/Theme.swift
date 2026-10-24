import SwiftUI
import UIKit

// The surfaces every screen sits on. Tokens live in LapisColors.swift,
// LapisType.swift and LapisMetrics.swift; the components in Components/.

/// A Lapis card: the card fill, a hairline edge, the soft card shadow.
struct CardBackground: ViewModifier {
    var fill: Color = Color.lapis.card
    var radius: CGFloat = Lapis.Radius.card
    var elevated = true

    func body(content: Content) -> some View {
        content
            .background {
                RoundedRectangle(cornerRadius: radius, style: .continuous)
                    .fill(fill)
                    .lapisShadow(elevated ? .card : .none)
            }
            .overlay(RoundedRectangle(cornerRadius: radius, style: .continuous).strokeBorder(Color.lapis.line, lineWidth: 1))
    }
}

/// A faint wash under the pointer (Mac, iPad with a trackpad); touch never sees it.
struct HoverWash: ViewModifier {
    var radius: CGFloat = Lapis.Radius.card
    @State private var hovering = false

    func body(content: Content) -> some View {
        content
            .overlay {
                RoundedRectangle(cornerRadius: radius, style: .continuous)
                    .fill(Color.lapis.ink.opacity(hovering ? 0.045 : 0))
                    .allowsHitTesting(false)
            }
            .onHover { hovering = $0 }
            .animation(Lapis.Motion.fast, value: hovering)
    }
}

extension View {
    /// A Lapis card (`fill` defaults to `card`; pass `card2` for an inset one).
    func card(_ fill: Color = Color.lapis.card, radius: CGFloat = Lapis.Radius.card, elevated: Bool = true) -> some View {
        modifier(CardBackground(fill: fill, radius: radius, elevated: elevated))
    }

    func hoverWash(radius: CGFloat = Lapis.Radius.card) -> some View { modifier(HoverWash(radius: radius)) }

    /// The screen ground behind scroll content and lists.
    func groundBackground() -> some View {
        background(Color.lapis.bg.ignoresSafeArea())
            .scrollContentBackground(.hidden)
    }
}
