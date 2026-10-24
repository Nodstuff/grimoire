import SwiftUI

/// The typographic cover a book without its own cover image wears: a
/// cloth colour, an ink colour and one quiet motif, picked from the book's
/// id so the same book always looks the same, on every client. A port of
/// the web's `ui/src/bookCovers.ts` (same 12 palettes, same FNV-1a hash
/// over UTF-16 code units). Artwork, not interface: covers keep their
/// colours in light and dark.
enum CoverArt {
    struct Art: Hashable, Sendable {
        var bg: UInt32
        var fg: UInt32
    }

    static let palettes: [Art] = [
        Art(bg: 0x1F3A4D, fg: 0xF1E6CF),
        Art(bg: 0xE6E0D2, fg: 0x1B2430),
        Art(bg: 0x2F5D46, fg: 0xEAF2DC),
        Art(bg: 0xF2C14E, fg: 0x2A1F05),
        Art(bg: 0xDCE3EA, fg: 0x1B2430),
        Art(bg: 0x2B2440, fg: 0xF4D9C6),
        Art(bg: 0x7A2E2A, fg: 0xF6E7D7),
        Art(bg: 0x23395B, fg: 0xE9EEF6),
        Art(bg: 0xC9622F, fg: 0xFFF1E3),
        Art(bg: 0xA9C3B5, fg: 0x16302A),
        Art(bg: 0x3B4A2A, fg: 0xE9DFB8),
        Art(bg: 0xF4F1EA, fg: 0xC2412D),
    ]

    /// FNV-1a over the id's UTF-16 code units (JS `charCodeAt`), as the web.
    static func hash(_ id: String) -> UInt32 {
        var h: UInt32 = 0x811C_9DC5
        for u in id.utf16 {
            h ^= UInt32(u)
            h = h &* 0x0100_0193
        }
        return h
    }

    static func index(for id: String) -> Int { Int(hash(id) % UInt32(palettes.count)) }
}

/// A book's cover: its image when it has one (loaded lazily, a skeleton
/// meanwhile), else the typographic cover. 2:3, corners and a hairline
/// like a printed jacket. Sized by its container's width.
struct BookCover: View {
    let id: String
    let title: String
    var author: String?
    var imageURL: URL?
    /// set by the Books screens: covers behind the server's bearer
    @Environment(\.coverImageLoader) private var loader

    var body: some View {
        Color.clear
            .aspectRatio(2.0 / 3.0, contentMode: .fit)
            .overlay {
                if let imageURL, let loader {
                    LoadedCoverImage(url: imageURL, loader: loader) {
                        TypographicCover(id: id, title: title, author: author)
                    }
                } else if let imageURL {
                    AsyncImage(url: imageURL, transaction: Transaction(animation: Lapis.Motion.standard)) { phase in
                        switch phase {
                        case let .success(image): image.resizable().scaledToFill()
                        case .failure: TypographicCover(id: id, title: title, author: author)
                        default: Rectangle().fill(Color.lapis.hover).shimmering()
                        }
                    }
                } else {
                    TypographicCover(id: id, title: title, author: author)
                }
            }
            .clipShape(.rect(cornerRadius: 4, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 4, style: .continuous).strokeBorder(Color.black.opacity(0.08), lineWidth: 0.5))
            .shadow(color: Color.lapis.shadow, radius: 6, y: 4)
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(author.map { "\(title), by \($0)" } ?? title)
            .accessibilityAddTraits(.isImage)
    }
}

/// Loads cover images the app has to ask for itself (the server's cover
/// route wants the bearer, which `AsyncImage` can't send).
struct CoverImageLoader: Sendable {
    let load: @Sendable (URL) async -> UIImage?
    /// what's already in memory, drawn at once (no shimmer flash)
    var cached: @Sendable (URL) -> UIImage? = { _ in nil }
}

extension EnvironmentValues {
    @Entry var coverImageLoader: CoverImageLoader?
}

/// A cover through `CoverImageLoader`: a shimmer while it loads, the
/// fallback when there is none.
struct LoadedCoverImage<Fallback: View>: View {
    let url: URL
    let loader: CoverImageLoader
    @ViewBuilder var fallback: Fallback
    @State private var phase: Phase = .loading
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    enum Phase: Equatable {
        case loading, missing
        case loaded(UIImage)
    }

    var body: some View {
        Group {
            switch phase {
            case let .loaded(image): Image(uiImage: image).resizable().scaledToFill()
            case .missing: fallback
            case .loading:
                if let image = loader.cached(url) {
                    Image(uiImage: image).resizable().scaledToFill()
                } else {
                    Rectangle().fill(Color.lapis.hover).shimmering()
                }
            }
        }
        .task(id: url) {
            let image = await loader.load(url)
            withAnimation(reduceMotion ? nil : Lapis.Motion.standard) { phase = image.map { .loaded($0) } ?? .missing }
        }
    }
}

/// The fallback cover: serif title, author, one motif.
struct TypographicCover: View {
    let id: String
    let title: String
    var author: String?

    var body: some View {
        let i = CoverArt.index(for: id)
        let art = CoverArt.palettes[i]
        let fg = Color(rgb: art.fg)
        GeometryReader { geo in
            let w = geo.size.width
            ZStack(alignment: .topLeading) {
                Color(rgb: art.bg)
                CoverMotif(index: i, ink: fg, size: geo.size)
                VStack(alignment: .leading, spacing: w * 0.04) {
                    Text(title)
                        .font(.newsreader(max(9, w * 0.115), weight: .semibold))
                        .tracking(-0.2)
                        .foregroundStyle(fg)
                        .lineLimit(5)
                        .minimumScaleFactor(0.7)
                    Spacer(minLength: 0)
                    if let author {
                        Text(author.uppercased())
                            .font(.system(size: max(6, w * 0.058), weight: .semibold))
                            .tracking(w * 0.006)
                            .foregroundStyle(fg.opacity(0.8))
                            .lineLimit(2)
                    }
                }
                .padding(w * 0.1)
                .frame(width: geo.size.width, height: geo.size.height, alignment: .topLeading)
            }
        }
        .clipped()
    }
}

/// The 12 motifs of the web's covers, drawn natively (same placements as
/// the CSS, in fractions of the cover).
private struct CoverMotif: View {
    let index: Int
    let ink: Color
    let size: CGSize

    var body: some View {
        let w = size.width, h = size.height
        switch index {
        case 0: // a broad ring off the bottom-right corner
            Circle().strokeBorder(ink.opacity(0.22), lineWidth: w * 0.07)
                .frame(width: w * 0.85, height: w * 0.85)
                .position(x: w + w * 0.2 - w * 0.425, y: h + w * 0.2 - w * 0.425)
        case 1: // vertical stripes in a band
            Canvas { ctx, s in
                var x: CGFloat = 0
                while x < s.width {
                    ctx.fill(Path(CGRect(x: x, y: 0, width: 3, height: s.height)), with: .color(ink.opacity(0.75)))
                    x += 11
                }
            }
            .frame(width: w - 32, height: h * 0.18)
            .position(x: w / 2, y: h * (1 - 0.22 - 0.09))
        case 2: // a leaf, one sharp corner
            UnevenRoundedRectangle(topLeadingRadius: w * 0.36, bottomLeadingRadius: w * 0.36, bottomTrailingRadius: 0, topTrailingRadius: w * 0.36)
                .fill(ink.opacity(0.16))
                .frame(width: w * 0.72, height: w * 0.72)
                .position(x: w + 20 - w * 0.36, y: h * 0.46 + w * 0.36)
        case 3: // a pale sun
            Circle().fill(Color.white.opacity(0.35))
                .frame(width: w * 0.92, height: w * 0.92)
                .position(x: w + 40 - w * 0.46, y: h * 0.3 + w * 0.46)
        case 4: // ruled lines
            Canvas { ctx, s in
                var y: CGFloat = 0
                while y < s.height {
                    ctx.fill(Path(CGRect(x: 0, y: y, width: s.width, height: 2)), with: .color(ink.opacity(0.12)))
                    y += 9
                }
            }
            .frame(width: w, height: h * 0.26)
            .position(x: w / 2, y: h * (1 - 0.15 - 0.13))
        case 5: // a small constellation
            ZStack {
                dot(6).position(x: w * 0.2 + 3, y: h * 0.52 + 3)
                dot(4).position(x: w * 0.2 + 33, y: h * 0.52 - 17)
                dot(2).position(x: w * 0.2 + 61, y: h * 0.52 + 13)
                dot(2).position(x: w * 0.2 + 17, y: h * 0.52 + 33)
            }
        case 6: // one hairline
            Rectangle().fill(ink.opacity(0.5))
                .frame(width: w - 32, height: 1)
                .position(x: w / 2, y: h * 0.48)
        case 7: // a horizon arc
            Ellipse().stroke(ink.opacity(0.3), lineWidth: 2)
                .frame(width: w + 40, height: h * 0.6)
                .position(x: w / 2, y: h + h * 0.3 - h * 0.3)
        case 8: // a ring and its echo
            ZStack {
                Circle().stroke(ink.opacity(0.2), lineWidth: 2)
                    .frame(width: w * 0.52 - 4, height: w * 0.52 - 4)
                    .offset(x: 24)
                Circle().stroke(ink.opacity(0.45), lineWidth: 2)
                    .frame(width: w * 0.52, height: w * 0.52)
            }
            .position(x: w / 2, y: h * 0.4 + w * 0.26)
        case 9: // an arched window
            UnevenRoundedRectangle(topLeadingRadius: 22, bottomLeadingRadius: 4, bottomTrailingRadius: 4, topTrailingRadius: 22)
                .fill(ink.opacity(0.14))
                .frame(width: w * 0.28, height: h * 0.3)
                .position(x: w - 18 - w * 0.14, y: h * 0.44 + h * 0.15)
        case 10: // a peak
            Path { p in
                let top = h * (1 - 0.18 - 0.32), bottom = h * (1 - 0.18), l: CGFloat = 10, r = w - 10
                p.move(to: CGPoint(x: l, y: bottom))
                p.addLine(to: CGPoint(x: (l + r) / 2, y: top))
                p.addLine(to: CGPoint(x: r, y: bottom))
                p.closeSubpath()
            }
            .fill(ink.opacity(0.3))
        default: // three rules
            Canvas { ctx, s in
                for y in [0, s.height / 2, s.height - 1] {
                    ctx.fill(Path(CGRect(x: 0, y: y, width: s.width, height: 1)), with: .color(ink.opacity(0.5)))
                }
            }
            .frame(width: w - 32, height: h * 0.3)
            .position(x: w / 2, y: h * (1 - 0.2 - 0.15))
        }
    }

    private func dot(_ d: CGFloat) -> some View {
        Circle().fill(ink).frame(width: d, height: d)
    }
}

extension Color {
    /// A fixed colour from `0xRRGGBB` (artwork; interface colours are tokens).
    init(rgb: UInt32) {
        self.init(uiColor: UIColor(rgb: rgb))
    }
}

#Preview("Book covers · light") {
    LapisPreview(title: "Book covers") { coversGallery }.preferredColorScheme(.light)
}

#Preview("Book covers · dark") {
    LapisPreview(title: "Book covers") { coversGallery }.preferredColorScheme(.dark)
}

@MainActor @ViewBuilder private var coversGallery: some View {
    let books = (0..<12).map { ("book-\($0)", ["The Name of the Rose", "Middlemarch", "A Wizard of Earthsea", "Piranesi", "The Left Hand of Darkness", "Stoner"][$0 % 6], ["Umberto Eco", "George Eliot", "Ursula K. Le Guin", "Susanna Clarke", "Ursula K. Le Guin", "John Williams"][$0 % 6]) }
    LazyVGrid(columns: [GridItem(.adaptive(minimum: 96), spacing: 14)], spacing: 14) {
        ForEach(books, id: \.0) { BookCover(id: $0.0, title: $0.1, author: $0.2) }
    }
}
