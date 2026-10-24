import SwiftUI
import UIKit

/// Lapis, the design language shared with the web (`ui/src/lapis/tokens.css`,
/// canvas artboard Lapis-System). Every colour on screen comes from here:
/// `Color.lapis.ink`, `.foregroundStyle(.lapis.ink3)`, `UIColor.lapis.accent`.
/// Never a hex in a screen.
///
/// Each token is a dynamic `UIColor`: it resolves against the trait
/// collection it is drawn in, so it follows the system appearance, the
/// app's own Appearance setting (a window override, `LapisAppearance`) and
/// Increase Contrast (quiet inks and hairlines step up one level).
enum Lapis {}

extension Lapis {
    /// One colour value: `0xRRGGBB` and an alpha.
    struct RGBA: Hashable, Sendable {
        var hex: UInt32
        var alpha: CGFloat = 1

        init(_ hex: UInt32, _ alpha: CGFloat = 1) {
            self.hex = hex
            self.alpha = alpha
        }

        var uiColor: UIColor {
            UIColor(
                red: CGFloat((hex >> 16) & 0xFF) / 255,
                green: CGFloat((hex >> 8) & 0xFF) / 255,
                blue: CGFloat(hex & 0xFF) / 255,
                alpha: alpha
            )
        }
    }

    /// Every Lapis colour, light and dark, exactly as `tokens.css` has them.
    /// A handful are native-only and say so.
    enum Token: String, CaseIterable, Sendable {
        // surfaces
        case bg, panel, card, card2, hover, pal, code, scrim, line, line2
        // ink
        case ink, ink2, ink3
        // lapis, the accent
        case accent, onAccent, accentSoft, accentRing
        // who wrote it
        case agent, agentSoft
        // verdicts: colour + icon + word, never colour alone
        case green, greenSoft, yellow, yellowMark, yellowSoft, mark, onYellow, red, redSoft
        /// native-only: solid verdict fills under white text (swipe actions,
        /// a filled destructive button), AA with white in both appearances
        case greenFill, yellowFill, redFill
        // workspace colours
        case ws1, ws2, ws3, ws4, ws5, ws6
        // shadows (the colour; offsets and blur are `LapisShadow`)
        case shadow, shadowPop

        var light: RGBA {
            switch self {
            case .bg: RGBA(0xF4F5F8)
            case .panel: RGBA(0xECEEF3)
            case .card: RGBA(0xFFFFFF)
            case .card2: RGBA(0xF6F7FA)
            case .hover: RGBA(0xE4E7EF)
            case .pal: RGBA(0xFFFFFF)
            case .code: RGBA(0xF1F3F7)
            case .scrim: RGBA(0x141A2D, 0.32)
            case .line: RGBA(0x141A2D, 0.09)
            case .line2: RGBA(0x141A2D, 0.17)
            case .ink: RGBA(0x141821)
            case .ink2: RGBA(0x454D5E)
            case .ink3: RGBA(0x5E6779)
            case .accent: RGBA(0x3346D3)
            case .onAccent: RGBA(0xFFFFFF)
            case .accentSoft: RGBA(0x3346D3, 0.09)
            case .accentRing: RGBA(0x3346D3, 0.25)
            case .agent: RGBA(0x7535CC)
            case .agentSoft: RGBA(0x7535CC, 0.09)
            case .green: RGBA(0x17804A)
            case .greenSoft: RGBA(0xDDF3E6)
            case .yellow: RGBA(0x8A5A00)
            case .yellowMark: RGBA(0xE2AE2E)
            case .yellowSoft: RGBA(0xFFF1C9)
            case .mark: RGBA(0xFFE7A3)
            case .onYellow: RGBA(0x1A1405)
            case .red: RGBA(0xB8202F)
            case .redSoft: RGBA(0xFBE0E3)
            case .greenFill: RGBA(0x17804A)
            case .yellowFill: RGBA(0x9A6500)
            case .redFill: RGBA(0xB8202F)
            case .ws1: RGBA(0x3346D3)
            case .ws2: RGBA(0x17804A)
            case .ws3: RGBA(0xD9692A)
            case .ws4: RGBA(0x7535CC)
            case .ws5: RGBA(0x0F7C8C)
            case .ws6: RGBA(0xC2336B)
            case .shadow: RGBA(0x141A2D, 0.07)
            case .shadowPop: RGBA(0x141A2D, 0.18)
            }
        }

        var dark: RGBA {
            switch self {
            case .bg: RGBA(0x0E1116)
            case .panel: RGBA(0x12161D)
            case .card: RGBA(0x181D26)
            case .card2: RGBA(0x1E2430)
            case .hover: RGBA(0x232A37)
            case .pal: RGBA(0x161B24)
            case .code: RGBA(0x0B0E13)
            case .scrim: RGBA(0x05070A, 0.62)
            case .line: RGBA(0xE2E8FF, 0.08)
            case .line2: RGBA(0xE2E8FF, 0.15)
            case .ink: RGBA(0xE8EBF2)
            case .ink2: RGBA(0xAEB6C6)
            case .ink3: RGBA(0x8D96A8)
            case .accent: RGBA(0x8E9DFF)
            case .onAccent: RGBA(0x0B0E1A)
            case .accentSoft: RGBA(0x8E9DFF, 0.14)
            case .accentRing: RGBA(0x8E9DFF, 0.35)
            case .agent: RGBA(0xC9A9FF)
            case .agentSoft: RGBA(0xC9A9FF, 0.13)
            case .green: RGBA(0x4FD18B)
            case .greenSoft: RGBA(0x4FD18B, 0.12)
            case .yellow: RGBA(0xF2C14E)
            case .yellowMark: RGBA(0xF2C14E)
            case .yellowSoft: RGBA(0xF2C14E, 0.13)
            case .mark: RGBA(0xF2C14E, 0.28)
            case .onYellow: RGBA(0x1A1405)
            case .red: RGBA(0xFF7A85)
            case .redSoft: RGBA(0xFF7A85, 0.12)
            case .greenFill: RGBA(0x1D7F4C)
            case .yellowFill: RGBA(0x936400)
            case .redFill: RGBA(0xC22F3D)
            case .ws1: RGBA(0x8E9DFF)
            case .ws2: RGBA(0x4FD18B)
            case .ws3: RGBA(0xF59E6B)
            case .ws4: RGBA(0xC9A9FF)
            case .ws5: RGBA(0x5CC8D6)
            case .ws6: RGBA(0xF27BA8)
            case .shadow: RGBA(0x000000, 0.28)
            case .shadowPop: RGBA(0x000000, 0.45)
            }
        }

        /// Increase Contrast: quiet inks step up a level, hairlines firm up,
        /// soft fills get denser. Everything else is unchanged.
        func value(dark isDark: Bool, highContrast: Bool) -> RGBA {
            guard highContrast else { return isDark ? dark : light }
            switch self {
            case .ink3: return Token.ink2.value(dark: isDark, highContrast: false)
            case .ink2: return isDark ? RGBA(0xD3D8E3) : RGBA(0x2E3443)
            case .line: return Token.line2.value(dark: isDark, highContrast: false)
            case .line2: return isDark ? RGBA(0xE2E8FF, 0.3) : RGBA(0x141A2D, 0.32)
            case .accentSoft, .agentSoft, .greenSoft, .yellowSoft, .redSoft:
                var v = isDark ? dark : light
                if v.alpha < 1 { v.alpha = min(1, v.alpha * 1.8) }
                return v
            default: return isDark ? dark : light
            }
        }

        /// The dynamic colour (cached: one provider per token).
        var uiColor: UIColor { Lapis.cache[self]! }

        var color: Color { Color(uiColor: uiColor) }

        fileprivate func makeDynamic() -> UIColor {
            UIColor { traits in
                value(dark: traits.userInterfaceStyle == .dark, highContrast: traits.accessibilityContrast == .high).uiColor
            }
        }
    }

    fileprivate static let cache: [Token: UIColor] = Dictionary(
        uniqueKeysWithValues: Token.allCases.map { ($0, $0.makeDynamic()) }
    )
}

// MARK: - SwiftUI

/// The tokens as SwiftUI colours: `Color.lapis.card`, `.foregroundStyle(.lapis.ink2)`.
struct LapisColors: Sendable {
    // surfaces
    /// the ground behind everything
    var bg: Color { Lapis.Token.bg.color }
    /// the sidebar
    var panel: Color { Lapis.Token.panel.color }
    var card: Color { Lapis.Token.card.color }
    /// inset fields, card footers, chips
    var card2: Color { Lapis.Token.card2.color }
    /// hover and pressed rows
    var hover: Color { Lapis.Token.hover.color }
    /// palettes, menus, popovers, dialogs
    var pal: Color { Lapis.Token.pal.color }
    var code: Color { Lapis.Token.code.color }
    var scrim: Color { Lapis.Token.scrim.color }
    /// a hairline
    var line: Color { Lapis.Token.line.color }
    /// a firmer line: outlines, field borders
    var line2: Color { Lapis.Token.line2.color }

    // ink
    var ink: Color { Lapis.Token.ink.color }
    /// secondary lines
    var ink2: Color { Lapis.Token.ink2.color }
    /// meta: counts, crumbs, timestamps, labels (AA on bg and card)
    var ink3: Color { Lapis.Token.ink3.color }

    // lapis
    var accent: Color { Lapis.Token.accent.color }
    var onAccent: Color { Lapis.Token.onAccent.color }
    var accentSoft: Color { Lapis.Token.accentSoft.color }
    var accentRing: Color { Lapis.Token.accentRing.color }

    // who wrote it
    var agent: Color { Lapis.Token.agent.color }
    var agentSoft: Color { Lapis.Token.agentSoft.color }

    // verdicts
    var green: Color { Lapis.Token.green.color }
    var greenSoft: Color { Lapis.Token.greenSoft.color }
    /// yellow as text (on yellowSoft, or the ground)
    var yellow: Color { Lapis.Token.yellow.color }
    /// a bare yellow mark: dots, rings, icons
    var yellowMark: Color { Lapis.Token.yellowMark.color }
    var yellowSoft: Color { Lapis.Token.yellowSoft.color }
    /// a search match
    var mark: Color { Lapis.Token.mark.color }
    var onYellow: Color { Lapis.Token.onYellow.color }
    var red: Color { Lapis.Token.red.color }
    var redSoft: Color { Lapis.Token.redSoft.color }
    /// solid fills under white text (native only)
    var greenFill: Color { Lapis.Token.greenFill.color }
    var yellowFill: Color { Lapis.Token.yellowFill.color }
    var redFill: Color { Lapis.Token.redFill.color }

    /// workspace colours 1…6 (blue, green, orange, violet, teal, pink)
    var workspace: [Color] { [Lapis.Token.ws1, .ws2, .ws3, .ws4, .ws5, .ws6].map(\.color) }

    var shadow: Color { Lapis.Token.shadow.color }
    var shadowPop: Color { Lapis.Token.shadowPop.color }
}

extension ShapeStyle where Self == Color {
    /// Lapis tokens: `.foregroundStyle(.lapis.ink)`, `Color.lapis.card`.
    static var lapis: LapisColors { LapisColors() }
}

// MARK: - UIKit

/// The tokens as dynamic UIColors, for UIKit (the editor, appearance proxies).
struct LapisUIColors: Sendable {
    subscript(_ token: Lapis.Token) -> UIColor { token.uiColor }

    var bg: UIColor { Lapis.Token.bg.uiColor }
    var card: UIColor { Lapis.Token.card.uiColor }
    var card2: UIColor { Lapis.Token.card2.uiColor }
    var code: UIColor { Lapis.Token.code.uiColor }
    var line: UIColor { Lapis.Token.line.uiColor }
    var line2: UIColor { Lapis.Token.line2.uiColor }
    var ink: UIColor { Lapis.Token.ink.uiColor }
    var ink2: UIColor { Lapis.Token.ink2.uiColor }
    var ink3: UIColor { Lapis.Token.ink3.uiColor }
    var accent: UIColor { Lapis.Token.accent.uiColor }
    var accentSoft: UIColor { Lapis.Token.accentSoft.uiColor }
    var agent: UIColor { Lapis.Token.agent.uiColor }
    var green: UIColor { Lapis.Token.green.uiColor }
    var yellow: UIColor { Lapis.Token.yellow.uiColor }
    var yellowMark: UIColor { Lapis.Token.yellowMark.uiColor }
    var yellowSoft: UIColor { Lapis.Token.yellowSoft.uiColor }
    var mark: UIColor { Lapis.Token.mark.uiColor }
    var red: UIColor { Lapis.Token.red.uiColor }
    var redSoft: UIColor { Lapis.Token.redSoft.uiColor }
}

extension UIColor {
    /// Lapis tokens for UIKit: `UIColor.lapis.ink`, `UIColor.lapis[.ws3]`.
    static var lapis: LapisUIColors { LapisUIColors() }

    convenience init(rgb: UInt32) {
        self.init(
            red: CGFloat((rgb >> 16) & 0xFF) / 255,
            green: CGFloat((rgb >> 8) & 0xFF) / 255,
            blue: CGFloat(rgb & 0xFF) / 255,
            alpha: 1
        )
    }
}

extension Color {
    /// A fixed light/dark pair outside the token set (artwork, chart series).
    init(dark: UInt32, light: UInt32) {
        self.init(uiColor: UIColor { traits in
            UIColor(rgb: traits.userInterfaceStyle == .dark ? dark : light)
        })
    }
}
