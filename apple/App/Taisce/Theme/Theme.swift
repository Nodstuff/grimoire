import SwiftUI
import UIKit

/// The design tokens. Dark-first; the light values are the same roles
/// inverted, and both follow the system appearance.
enum Theme {
    static let ground = Color(dark: 0x101014, light: 0xF6F6F8)
    static let surface = Color(dark: 0x17171D, light: 0xFFFFFF)
    static let surface2 = Color(dark: 0x1E1E26, light: 0xEDEDF2)
    static let hairline = Color(dark: 0x26262F, light: 0xDCDCE3)
    static let text = Color(dark: 0xD6D6DD, light: 0x1C1C22)
    /// AA on `ground` in both appearances
    static let secondary = Color(dark: 0x8F8F9E, light: 0x5E5E6C)
    static let accent = Color(dark: 0x8B9DC3, light: 0x4A5E8E)
    /// links and the active state
    static let accentActive = Color(dark: 0xA9B7D6, light: 0x3A4E7C)
    static let green = Color(dark: 0x95C99B, light: 0x3D7B47)
    static let amber = Color(dark: 0xD9B47A, light: 0x96661C)
    static let rose = Color(dark: 0xD98A94, light: 0xAE4757)

    static let radius: CGFloat = 14
    static let gutter: CGFloat = 20
    static let readingWidth: CGFloat = 680
    static let minTarget: CGFloat = 44

    /// New York for titles; everything else is SF Pro / SF Mono.
    static func serif(_ style: Font.TextStyle, weight: Font.Weight = .semibold) -> Font {
        .system(style, design: .serif).weight(weight)
    }

    static let mono = Font.system(.footnote, design: .monospaced)
}

extension Color {
    init(dark: UInt32, light: UInt32) {
        self.init(uiColor: UIColor { traits in
            UIColor(rgb: traits.userInterfaceStyle == .light ? light : dark)
        })
    }
}

extension UIColor {
    convenience init(rgb: UInt32) {
        self.init(
            red: CGFloat((rgb >> 16) & 0xFF) / 255,
            green: CGFloat((rgb >> 8) & 0xFF) / 255,
            blue: CGFloat(rgb & 0xFF) / 255,
            alpha: 1
        )
    }
}

/// A rounded surface card with a hairline edge.
struct CardBackground: ViewModifier {
    var fill: Color = Theme.surface

    func body(content: Content) -> some View {
        content
            .background(fill, in: .rect(cornerRadius: Theme.radius, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: Theme.radius, style: .continuous).strokeBorder(Theme.hairline, lineWidth: 1))
    }
}

extension View {
    func card(_ fill: Color = Theme.surface) -> some View { modifier(CardBackground(fill: fill)) }

    /// The screen ground behind scroll content and lists.
    func groundBackground() -> some View {
        background(Theme.ground.ignoresSafeArea())
            .scrollContentBackground(.hidden)
    }
}
