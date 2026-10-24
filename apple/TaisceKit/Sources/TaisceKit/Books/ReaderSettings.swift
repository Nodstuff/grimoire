import Foundation

/// The in-app reader's settings (`ui/src/reader/settings.ts`, the same
/// choices and ladders as the web reader), per device.
public struct ReaderSettings: Codable, Sendable, Hashable {
    public enum Paper: String, Codable, Sendable, CaseIterable, Identifiable {
        /// the app's own light or dark
        case auto, light, sepia, dark
        public var id: String { rawValue }
        public var title: String {
            switch self {
            case .auto: "Auto"
            case .light: "Light"
            case .sepia: "Sepia"
            case .dark: "Dark"
            }
        }
    }

    public enum Flow: String, Codable, Sendable, CaseIterable, Identifiable {
        case paginated, scrolled
        public var id: String { rawValue }
        public var title: String { self == .paginated ? "Pages" : "Scroll" }
    }

    public enum Margins: String, Codable, Sendable, CaseIterable, Identifiable {
        case narrow, normal, wide
        public var id: String { rawValue }
        public var title: String { rawValue.capitalized }
    }

    public enum Face: String, Codable, Sendable, CaseIterable, Identifiable {
        /// Newsreader, the app's reading face
        case newsreader
        /// the book's own fonts
        case book
        public var id: String { rawValue }
        public var title: String { self == .newsreader ? "Newsreader" : "Book's own" }
    }

    public var paper: Paper
    /// percent of the book's base size (`fontSizes`)
    public var fontSize: Int
    /// `lineHeights`
    public var lineHeight: Double
    public var margins: Margins
    public var flow: Flow
    public var face: Face

    public static let fontSizes = [80, 90, 100, 110, 120, 135, 150, 170]
    public static let lineHeights = [1.3, 1.45, 1.55, 1.7, 1.9]
    public static let defaults = ReaderSettings(paper: .auto, fontSize: 100, lineHeight: 1.55, margins: .normal, flow: .paginated, face: .newsreader)

    public init(paper: Paper, fontSize: Int, lineHeight: Double, margins: Margins, flow: Flow, face: Face) {
        self.paper = paper
        self.fontSize = fontSize
        self.lineHeight = lineHeight
        self.margins = margins
        self.flow = flow
        self.face = face
    }

    /// Unknown or out-of-ladder values fall back to the defaults, field by
    /// field (a stored setting from another version never breaks the reader).
    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let d = Self.defaults
        paper = (try? c.decode(Paper.self, forKey: .paper)) ?? d.paper
        let size = (try? c.decode(Int.self, forKey: .fontSize)) ?? d.fontSize
        fontSize = Self.fontSizes.contains(size) ? size : d.fontSize
        let lh = (try? c.decode(Double.self, forKey: .lineHeight)) ?? d.lineHeight
        lineHeight = Self.lineHeights.contains(lh) ? lh : d.lineHeight
        margins = (try? c.decode(Margins.self, forKey: .margins)) ?? d.margins
        flow = (try? c.decode(Flow.self, forKey: .flow)) ?? d.flow
        face = (try? c.decode(Face.self, forKey: .face)) ?? d.face
    }

    /// One step up or down a ladder (clamped): `step(fontSizes, 100, +1)` = 110.
    public static func step<T: Equatable>(_ ladder: [T], _ cur: T, _ dir: Int) -> T {
        let i = ladder.firstIndex(of: cur) ?? 0
        return ladder[min(ladder.count - 1, max(0, i + dir))]
    }

    public var canGrow: Bool { fontSize != Self.fontSizes.last }
    public var canShrink: Bool { fontSize != Self.fontSizes.first }

    /// The paper actually shown: Auto follows the app's appearance.
    public func resolvedPaper(dark: Bool) -> Paper {
        paper == .auto ? (dark ? .dark : .light) : paper
    }

    /// The paginator's layout for a margin choice (`layoutFor`).
    public func layout(phone: Bool) -> (gap: String, margin: String, maxInline: String) {
        if phone {
            return (margins == .narrow ? "4%" : margins == .wide ? "12%" : "7%", "56px", "720px")
        }
        switch margins {
        case .narrow: return ("4%", "56px", "860px")
        case .wide: return ("10%", "72px", "600px")
        case .normal: return ("7%", "64px", "720px")
        }
    }

    /// What the reader page is handed (`window.taisce.setLook`).
    public func look(dark: Bool, phone: Bool, reduceMotion: Bool) -> ReaderLook {
        let p = ReaderPaperColours.of(resolvedPaper(dark: dark))
        let l = layout(phone: phone)
        return ReaderLook(paper: p.paper, ink: p.ink, link: p.link, mark: p.select, scheme: p.dark ? "dark" : "light",
                          fontSize: fontSize, lineHeight: lineHeight, gap: l.gap, margin: l.margin, maxInline: l.maxInline,
                          flow: flow.rawValue, face: face.rawValue, animated: !reduceMotion)
    }
}

/// The page's look as the reader script takes it (`main.ts` `Look`).
public struct ReaderLook: Codable, Sendable, Hashable {
    public var paper: String
    public var ink: String
    public var link: String
    public var mark: String
    public var scheme: String
    public var fontSize: Int
    public var lineHeight: Double
    public var gap: String
    public var margin: String
    public var maxInline: String
    public var flow: String
    public var face: String
    public var animated: Bool
}

/// The reader's papers: the page colours of `.rd[data-paper=…]` in
/// `ui/src/lapis/tokens.css` (`--read-paper`, `--read-ink`, `--read-link`,
/// `--read-select`), so a book reads the same in the browser and the app.
/// The book's page is web content: these are CSS values, not app colours.
public struct ReaderPaperColours: Sendable, Hashable {
    public var paper: String
    public var ink: String
    public var link: String
    public var select: String
    public var dark: Bool

    public static func of(_ p: ReaderSettings.Paper) -> ReaderPaperColours {
        switch p {
        case .light, .auto: ReaderPaperColours(paper: "#ffffff", ink: "#1b1f29", link: "#3346d3", select: "rgba(51, 70, 211, 0.2)", dark: false)
        case .dark: ReaderPaperColours(paper: "#12161d", ink: "#d9dde6", link: "#a3afff", select: "rgba(142, 157, 255, 0.3)", dark: true)
        case .sepia: ReaderPaperColours(paper: "#f4ecd8", ink: "#3b2f1f", link: "#7a4b16", select: "rgba(160, 110, 40, 0.25)", dark: false)
        }
    }

    /// `#rrggbb` as 0…1 components (the native chrome around the page).
    public static func rgb(_ hex: String) -> (r: Double, g: Double, b: Double)? {
        let h = hex.hasPrefix("#") ? String(hex.dropFirst()) : hex
        guard h.count == 6, let v = UInt32(h, radix: 16) else { return nil }
        return (Double((v >> 16) & 0xFF) / 255, Double((v >> 8) & 0xFF) / 255, Double(v & 0xFF) / 255)
    }
}

/// The reader's progress line under the page.
public enum ReaderFormat {
    /// "12 min left in chapter" from foliate's estimate, or nil (`minutesLeft`).
    public static func minutesLeft(_ m: Double?) -> String? {
        guard let m, m.isFinite else { return nil }
        let n = Int(m.rounded())
        if n < 1 { return m > 0 ? "Less than a minute left in chapter" : nil }
        return "\(n) min left in chapter"
    }

    /// 0…100 from a 0…1 fraction (`percentOf`).
    public static func percent(_ fraction: Double?) -> Int {
        guard let f = fraction, f.isFinite else { return 0 }
        return Int((min(1, max(0, f)) * 100).rounded())
    }
}
