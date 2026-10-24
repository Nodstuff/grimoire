import SwiftUI
import TaisceKit
import UIKit

/// A book in the grid: its cover (with "NEW" while unopened and recent),
/// the title, and where you are in it.
struct BookTile: View {
    let book: Book
    let state: String
    var isNew = false
    var selected = false
    var coverURL: URL?

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            BookCover(id: book.id, title: book.title, author: book.authorName, imageURL: coverURL)
                .overlay(alignment: .topTrailing) {
                    // on the corner, clear of a cover's own title
                    if isNew { NewBadge().offset(x: 6, y: -7) }
                }
                .overlay {
                    if selected {
                        RoundedRectangle(cornerRadius: 6, style: .continuous)
                            .strokeBorder(Color.lapis.accent, lineWidth: 3)
                            .padding(-5)
                    }
                }
            VStack(alignment: .leading, spacing: 2) {
                // two lines, as the artboards ("A Tale of Two Cities" whole)
                Text(book.title)
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(Color.lapis.ink)
                    .lineLimit(2)
                    .multilineTextAlignment(.leading)
                    .fixedSize(horizontal: false, vertical: true)
                Text(state)
                    .font(.footnote)
                    .foregroundStyle(Color.lapis.ink3)
                    .lineLimit(1)
            }
        }
        .contentShape(.rect)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel([book.title, book.authorName.map { "by \($0)" }, isNew ? "new" : nil, state].compactMap { $0 }.joined(separator: ", "))
        .accessibilityAddTraits(selected ? [.isButton, .isSelected] : .isButton)
    }
}

/// "64% · Kindle · 2 h ago" on one line when it fits; else "64% · Kindle"
/// over "2 h ago", each phrase whole (never "2 / min ago").
struct ProgressLine: View {
    let progress: BookProgress
    var big = false

    var body: some View {
        let pct = Text(BookFormat.percent(progress.percentage)).fontWeight(.semibold).foregroundStyle(Color.lapis.ink)
        let device = progress.device.isEmpty ? "Another device" : progress.device
        let when = progress.updatedAt.map { BookFormat.ago($0) }
        ViewThatFits(in: .horizontal) {
            Text("\(pct) \u{00B7} \(BookFormat.deviceLine(progress))").lineLimit(1)
            VStack(alignment: .leading, spacing: 1) {
                Text("\(pct) \u{00B7} \(device)").lineLimit(big ? nil : 1)
                if let when { Text(when).lineLimit(1) }
            }
        }
        .foregroundStyle(Color.lapis.ink3)
        .font(.footnote)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(BookFormat.percent(progress.percentage)) read, \(BookFormat.deviceLine(progress))")
    }
}

/// "NEW" on a cover added this week and not opened yet.
struct NewBadge: View {
    var body: some View {
        Text("NEW")
            .font(.caption2.weight(.bold))
            .tracking(0.6)
            .foregroundStyle(Color.lapis.onAccent)
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .background(Color.lapis.accent, in: .capsule)
            .accessibilityHidden(true)
    }
}

/// A Continue reading card: cover, series, title, author, progress, and
/// the button that opens the book where you left it.
struct ReadingCard: View {
    let book: Book
    let progress: BookProgress
    var coverURL: URL?
    var compact = false
    var selected = false
    let onContinue: () -> Void
    var onOpen: () -> Void = {}
    @Environment(\.dynamicTypeSize) private var typeSize

    /// accessibility sizes: the cover above the words, nothing cut short
    private var big: Bool { typeSize.isAccessibilitySize }

    var body: some View {
        let layout = big ? AnyLayout(VStackLayout(alignment: .leading, spacing: Lapis.Space.s3))
            : AnyLayout(HStackLayout(alignment: .top, spacing: Lapis.Space.s4))
        layout {
            BookCover(id: book.id, title: book.title, author: book.authorName, imageURL: coverURL)
                .frame(width: compact ? 80 : 96)
            VStack(alignment: .leading, spacing: 4) {
                if let series = BookFormat.seriesLine(book) {
                    Text(series.uppercased())
                        .font(.caption.weight(.semibold))
                        .tracking(0.6)
                        .foregroundStyle(Color.lapis.ink3)
                        .lineLimit(1)
                }
                Text(book.title)
                    .lapisFont(.docHeading3)
                    .foregroundStyle(Color.lapis.ink)
                    .lineLimit(big ? nil : 2)
                    .fixedSize(horizontal: false, vertical: true)
                if let author = book.authorName {
                    Text(author).font(.subheadline).foregroundStyle(Color.lapis.ink2).lineLimit(big ? nil : 1)
                }
                Spacer(minLength: big ? 4 : 8)
                ProgressBar(value: progress.percentage / 100)
                ProgressLine(progress: progress, big: big)
                    .padding(.top, 2)
                if compact {
                    Button(action: onContinue) {
                        Label("Continue", systemImage: "book")
                    }
                    .buttonStyle(.lapis(.primary, fullWidth: big))
                    .accessibilityHint("Opens the book where you left off")
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding(Lapis.Space.s4)
        .frame(maxWidth: .infinity, alignment: .leading)
        .card()
        .overlay {
            if selected {
                RoundedRectangle(cornerRadius: Lapis.Radius.card, style: .continuous).strokeBorder(Color.lapis.accent, lineWidth: 2)
            }
        }
        .hoverWash()
        .contentShape(.rect(cornerRadius: Lapis.Radius.card))
        .onTapGesture(perform: onOpen)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(book.title), \(BookFormat.percent(progress.percentage)) read, \(BookFormat.deviceLine(progress))")
        .accessibilityAction(named: "Continue reading", onContinue)
    }
}

/// A shelf chip with its count: "All 128", "The Horus Heresy 54".
struct ShelfChip: View {
    let title: String
    let count: Int
    let selected: Bool
    let action: () -> Void

    var body: some View {
        Button {
            Haptics.selection()
            action()
        } label: {
            HStack(spacing: 6) {
                Text(title).font(.subheadline.weight(selected ? .semibold : .regular))
                Text("\(count)").font(.subheadline).monospacedDigit().opacity(0.7)
            }
            .foregroundStyle(selected ? Color.lapis.bg : Color.lapis.ink)
            .padding(.horizontal, 14)
            .frame(minHeight: 36)
            .background(selected ? Color.lapis.ink : Color.lapis.card, in: .capsule)
            .overlay(Capsule().strokeBorder(selected ? .clear : Color.lapis.line, lineWidth: 1))
            .frame(minHeight: Lapis.Layout.minTarget)
            .contentShape(.rect)
            .animation(Lapis.Motion.fast, value: selected)
        }
        .buttonStyle(.plain)
        .accessibilityLabel("\(title), \(count) books")
        .accessibilityAddTraits(selected ? .isSelected : [])
    }
}

/// The colour a cover is mostly: the detail's wash and the reader's frame.
enum CoverTint {
    /// The fallback cover's cloth.
    static func typographic(_ id: String) -> Color {
        Color(rgb: CoverArt.palettes[CoverArt.index(for: id)].bg)
    }

    /// An image's average colour, darkened a touch so a wash under ink reads.
    static func average(_ image: UIImage) -> Color? {
        guard let cg = image.cgImage else { return nil }
        let w = 8, h = 12
        var px = [UInt8](repeating: 0, count: w * h * 4)
        guard let ctx = CGContext(data: &px, width: w, height: h, bitsPerComponent: 8, bytesPerRow: w * 4,
                                  space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
        else { return nil }
        ctx.interpolationQuality = .medium
        ctx.draw(cg, in: CGRect(x: 0, y: 0, width: w, height: h))
        var r = 0, g = 0, b = 0
        for i in stride(from: 0, to: px.count, by: 4) {
            r += Int(px[i]); g += Int(px[i + 1]); b += Int(px[i + 2])
        }
        let n = Double(w * h) * 255
        return Color(red: Double(r) / n * 0.85, green: Double(g) / n * 0.85, blue: Double(b) / n * 0.85)
    }
}

/// The book's cover colour once its image is in (or the fallback's cloth).
struct CoverTintReader: ViewModifier {
    let book: Book
    let url: URL?
    @Binding var tint: Color?
    @Environment(\.coverImageLoader) private var loader

    func body(content: Content) -> some View {
        content.task(id: url) {
            guard let url, let loader, let img = await loader.load(url) else {
                tint = CoverTint.typographic(book.id)
                return
            }
            tint = CoverTint.average(img) ?? CoverTint.typographic(book.id)
        }
    }
}

/// A transient UI file to hand the share sheet (Open in Books, AirDrop, Files).
struct ShareFile: Identifiable {
    let id = UUID()
    let url: URL
}

/// The system share sheet over one file.
struct ActivitySheet: UIViewControllerRepresentable {
    let items: [Any]

    func makeUIViewController(context: Context) -> UIActivityViewController {
        UIActivityViewController(activityItems: items, applicationActivities: nil)
    }

    func updateUIViewController(_ vc: UIActivityViewController, context: Context) {}
}
