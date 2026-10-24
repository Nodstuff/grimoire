import SwiftUI
import Testing
import TaisceKit
import UIKit
@testable import Taisce

/// The Lapis foundation: tokens, type, the pure parts of components.
struct LapisTests {
    private func resolved(_ token: Lapis.Token, _ style: UIUserInterfaceStyle, contrast: UIAccessibilityContrast = .normal) -> UIColor {
        token.uiColor.resolvedColor(with: UITraitCollection { $0.userInterfaceStyle = style; $0.accessibilityContrast = contrast })
    }

    private func hex(_ c: UIColor) -> String {
        var r: CGFloat = 0, g: CGFloat = 0, b: CGFloat = 0, a: CGFloat = 0
        c.getRed(&r, green: &g, blue: &b, alpha: &a)
        return String(format: "%02X%02X%02X@%.2f", Int((r * 255).rounded()), Int((g * 255).rounded()), Int((b * 255).rounded()), a)
    }

    @Test func tokensMatchTheWebInBothAppearances() {
        // spot checks against ui/src/lapis/tokens.css
        #expect(hex(resolved(.bg, .light)) == "F4F5F8@1.00")
        #expect(hex(resolved(.bg, .dark)) == "0E1116@1.00")
        #expect(hex(resolved(.accent, .light)) == "3346D3@1.00")
        #expect(hex(resolved(.accent, .dark)) == "8E9DFF@1.00")
        #expect(hex(resolved(.agent, .dark)) == "C9A9FF@1.00")
        #expect(hex(resolved(.line, .light)) == "141A2D@0.09")
        #expect(hex(resolved(.yellowSoft, .light)) == "FFF1C9@1.00")
        #expect(hex(resolved(.redSoft, .dark)) == "FF7A85@0.12")
    }

    @Test func everyTokenAdaptsExceptTheFixedOnes() {
        let same: Set<Lapis.Token> = [.onYellow]
        for t in Lapis.Token.allCases where !same.contains(t) {
            #expect(t.light != t.dark, "\(t) has one value for light and dark")
        }
    }

    @Test func increaseContrastStepsQuietInkUp() {
        #expect(hex(resolved(.ink3, .light, contrast: .high)) == hex(resolved(.ink2, .light)))
        #expect(hex(resolved(.line, .dark, contrast: .high)) == hex(resolved(.line2, .dark)))
        #expect(hex(resolved(.accent, .light, contrast: .high)) == hex(resolved(.accent, .light)))
    }

    @Test func solidFillsCarryWhiteTextAtAA() {
        func luminance(_ c: UIColor) -> CGFloat {
            var r: CGFloat = 0, g: CGFloat = 0, b: CGFloat = 0, a: CGFloat = 0
            c.getRed(&r, green: &g, blue: &b, alpha: &a)
            func lin(_ v: CGFloat) -> CGFloat { v <= 0.03928 ? v / 12.92 : pow((v + 0.055) / 1.055, 2.4) }
            return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
        }
        for t in [Lapis.Token.greenFill, .yellowFill, .redFill] {
            for style in [UIUserInterfaceStyle.light, .dark] {
                let ratio = 1.05 / (luminance(resolved(t, style)) + 0.05)
                #expect(ratio >= 4.5, "\(t) \(style.rawValue): \(ratio)")
            }
        }
    }

    @Test func appearanceReadsWhatWasStored() {
        #expect(LapisAppearance(stored: nil) == .system)
        #expect(LapisAppearance(stored: "dark") == .dark)
        #expect(LapisAppearance(stored: "sepia") == .system)
        #expect(LapisAppearance.dark.interfaceStyle == .dark)
        #expect(LapisAppearance.system.interfaceStyle == .unspecified)
        // a device preference: sign-out doesn't forget it
        #expect(!UserSettings(server: "https://example.org").keys.contains(UserSettings.appearanceKey))
    }

    @Test func newsreaderIsBundledAndVariable() throws {
        #expect(Newsreader.isAvailable)
        let regular = Newsreader.uiFont(size: 18)
        let bold = Newsreader.uiFont(size: 18, weight: .semibold)
        #expect(regular.familyName == "Newsreader")
        #expect(regular.pointSize == 18)
        let axes = try #require(CTFontCopyVariation(bold as CTFont) as? [Int: CGFloat])
        #expect(axes[0x7767_6874] == 600) // wght
        // opsz follows the size (18, the font's default, isn't reported)
        let title = try #require(CTFontCopyVariation(Newsreader.uiFont(size: 34) as CTFont) as? [Int: CGFloat])
        #expect(title[0x6F70_737A] == 34)
        #expect(Newsreader.uiFont(size: 18, italic: true).fontName.contains("Italic"))
    }

    @Test func serifStylesScaleWithDynamicTypeAndDocScale() {
        let large = UITraitCollection { $0.preferredContentSizeCategory = .large }
        let xxl = UITraitCollection { $0.preferredContentSizeCategory = .accessibilityLarge }
        let mac = UITraitCollection { $0.preferredContentSizeCategory = .large; $0.docScale = 2 }
        let body = LapisTextStyle.docBody
        #expect(body.uiFont(traits: large).pointSize == 18)
        #expect(body.uiFont(traits: xxl).pointSize > 18)
        #expect(body.uiFont(traits: mac).pointSize == 36)
        let bold = UITraitCollection { $0.legibilityWeight = .bold }
        let axes = CTFontCopyVariation(body.uiFont(traits: bold) as CTFont) as? [Int: CGFloat]
        #expect(axes?[0x7767_6874] == 500) // Bold Text: one step heavier
    }

    @Test func verdictsReadWithoutColour() {
        #expect(Verdict(lapis: "parked") == .red && Verdict(lapis: "yellow") == .yellow && Verdict(lapis: "Applied") == .green)
        #expect(Verdict(lapis: "maybe") == nil)
        // three different shapes, three different words
        #expect(Set([Verdict.green, .yellow, .red].map(\.symbol)).count == 3)
        #expect(Verdict.yellow.longWord == "Flagged · applied" && Verdict.red.longWord == "Parked · not applied")
    }

    @Test func wordDiffKeepsBothSidesWhole() throws {
        let old = "The sync engine retries every 5 seconds."
        let new = "The sync engine backs off from 1 to 30 seconds, with jitter."
        let segs = WordDiff.segments(old: old, new: new)
        #expect(segs.filter { $0.kind != .added }.map(\.text).joined() == old)
        #expect(segs.filter { $0.kind != .removed }.map(\.text).joined() == new)
        #expect(segs.first == WordDiff.Segment(kind: .same, text: "The sync engine "))
        // a removal comes before its addition
        let firstChange = try #require(segs.firstIndex { $0.kind != .same })
        #expect(segs[firstChange].kind == .removed)
        #expect(WordDiff.segments(old: "", new: "Hello") == [.init(kind: .added, text: "Hello")])
        #expect(WordDiff.segments(old: "same", new: "same") == [.init(kind: .same, text: "same")])
    }

    @Test func coverArtMatchesTheWeb() {
        // FNV-1a values computed by ui/src/bookCovers.ts's hash()
        #expect(CoverArt.hash("") == 2_166_136_261)
        #expect(CoverArt.hash("book-1") == 2_430_922_394)
        #expect(CoverArt.hash("01J9ZK3Q8X") == 374_499_504)
        #expect(CoverArt.hash("Ériu 🐉") == 1_780_297_622) // UTF-16 code units, surrogates included
        #expect(CoverArt.index(for: "a") == 4)
        #expect(CoverArt.palettes.count == 12)
    }

    @Test func deadlinePillFromDueLabel() {
        #expect(DeadlinePill(DueLabel(tone: .overdue, text: "Yesterday"))?.kind == .overdue)
        #expect(DeadlinePill(DueLabel(tone: .later, text: "Fri"))?.kind == .upcoming)
        #expect(DeadlinePill(DueLabel(tone: .none, text: nil)) == nil)
    }
}
