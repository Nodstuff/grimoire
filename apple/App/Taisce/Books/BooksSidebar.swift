import SwiftUI
import TaisceKit

/// The iPad/Mac sidebar under Books while Books is in front (Mac-Books,
/// iPad-Books): SHELVES and SERIES pick the shelf the grid shows (the
/// router's `bookShelf`, `BooksScreen(filter:)`), E-READERS lists the paired
/// devices and when each last synced (a click opens Settings › E-readers).
struct BooksSidebarSections: View {
    @Environment(AppModel.self) private var model
    @Environment(Router.self) private var router

    private var store: BookStore { model.books }

    var body: some View {
        let shelf = store.shelf
        VStack(alignment: .leading, spacing: 2) {
            label("Shelves")
            ForEach(BookShelfFilter.shelves, id: \.self) { f in
                let n = shelf.count(f)
                if n > 0 || f == .reading { row(f.title, count: n, filter: f) }
            }
            if !shelf.series.isEmpty {
                label("Series")
                ForEach(shelf.series.prefix(12), id: \.name) { s in
                    row(s.name, count: s.count, filter: .series(s.name))
                }
            }
            if !store.ereaders.isEmpty {
                label("E-readers")
                ForEach(store.ereaders) { e in
                    BookSidebarRow(title: e.label, trailing: e.lastUsedAt.flatMap(RFC3339.date).map { Self.since($0) },
                                   selected: false, identifier: "sidebar.ereader") {
                        router.showSettings = true
                    }
                    .accessibilityHint("Opens Settings")
                }
            }
        }
        .transition(.opacity)
    }

    private func label(_ text: String) -> some View {
        Text(text.uppercased())
            .lapisFont(.label)
            .foregroundStyle(Color.lapis.ink3)
            .padding(.leading, 10)
            .padding(.top, 14)
            .padding(.bottom, 4)
            .accessibilityLabel(text)
            .accessibilityAddTraits(.isHeader)
    }

    private func row(_ title: String, count: Int, filter: BookShelfFilter) -> some View {
        let on = router.bookShelf == filter && router.sidebarSelection == .books
        return BookSidebarRow(title: title, trailing: "\(count)", selected: on, identifier: "sidebar.shelf") {
            // a second click on the shelf showing goes back to every book
            router.bookShelf = on ? .all : filter
            if router.sidebarSelection != .books { router.select(.books) }
        }
    }

    /// "2 h", "3 d", "5 min", "now": the sidebar's short since.
    static func since(_ at: Date, now: Date = .now) -> String {
        let s = max(0, now.timeIntervalSince(at))
        if s < 60 { return "now" }
        if s < 3600 { return "\(Int(s / 60)) min" }
        if s < 86_400 { return "\(Int(s / 3600)) h" }
        if s < 30 * 86_400 { return "\(Int(s / 86_400)) d" }
        return at.formatted(.dateTime.day().month(.abbreviated))
    }
}

/// A shelf, series or e-reader under Books: indented past the icons above,
/// the count (or time) quiet at the end.
private struct BookSidebarRow: View {
    let title: String
    var trailing: String?
    let selected: Bool
    let identifier: String
    let action: () -> Void
    @State private var pointer = false

    var body: some View {
        HStack(spacing: 8) {
            Text(title).foregroundStyle(Color.lapis.ink).lineLimit(1)
            Spacer(minLength: 0)
            if let trailing {
                Text(trailing).font(.subheadline).monospacedDigit().foregroundStyle(Color.lapis.ink3)
            }
        }
        .padding(.leading, 44)
        .padding(.trailing, 10)
        .frame(maxWidth: .infinity, minHeight: 36, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: Lapis.Radius.field, style: .continuous)
            .fill(selected ? Color.lapis.hover : pointer ? Color.lapis.hover.opacity(0.6) : .clear))
        .contentShape(.rect)
        .onHover { pointer = $0 }
        .onTapGesture(perform: action)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(trailing.map { "\(title), \($0)" } ?? title)
        .accessibilityAddTraits(selected ? [.isButton, .isSelected] : .isButton)
        .accessibilityAction { action() }
        .accessibilityIdentifier(identifier)
    }
}
