import SwiftUI
import TaisceKit

/// A small uppercase section label ("DUE", "PINNED").
struct SectionLabel: View {
    let title: String
    var color: Color = Theme.secondary
    var trailing: AnyView?

    init(_ title: String, color: Color = Theme.secondary, trailing: AnyView? = nil) {
        self.title = title
        self.color = color
        self.trailing = trailing
    }

    var body: some View {
        HStack(alignment: .firstTextBaseline) {
            Text(title.uppercased())
                .font(.footnote.weight(.semibold))
                .tracking(1.2)
                .foregroundStyle(color)
                .accessibilityAddTraits(.isHeader)
            Spacer()
            trailing
        }
        .padding(.horizontal, 4)
    }
}

/// The big serif screen title with an optional line above it.
struct ScreenHeader<Trailing: View>: View {
    let title: String
    var kicker: String?
    @ViewBuilder var trailing: Trailing

    var body: some View {
        HStack(alignment: .bottom) {
            VStack(alignment: .leading, spacing: 2) {
                if let kicker {
                    Text(kicker).font(.subheadline).foregroundStyle(Theme.secondary)
                }
                Text(title)
                    .font(Theme.serif(.largeTitle))
                    .foregroundStyle(Theme.text)
                    .accessibilityAddTraits(.isHeader)
            }
            Spacer(minLength: 12)
            trailing
        }
    }
}

extension ScreenHeader where Trailing == EmptyView {
    init(title: String, kicker: String? = nil) {
        self.init(title: title, kicker: kicker) { EmptyView() }
    }
}

/// The to-do status ring: rose overdue, amber due today, grey otherwise.
struct TodoRing: View {
    let tone: DueLabel.Tone
    var done = false

    var color: Color {
        if done { return Theme.green }
        return switch tone {
        case .overdue: Theme.rose
        case .today: Theme.amber
        case .later, .none: Theme.secondary
        }
    }

    var body: some View {
        ZStack {
            Circle().strokeBorder(color, lineWidth: 2)
            if done {
                Image(systemName: "checkmark").font(.system(size: 11, weight: .bold)).foregroundStyle(color)
            }
        }
        .frame(width: 22, height: 22)
        .accessibilityHidden(true)
    }
}

/// One to-do row inside a card list.
struct TodoRow: View {
    let entry: TodoEntry
    var now: Date = .now
    var done = false

    var body: some View {
        let label = DueLabel.make(entry, now: now)
        HStack(alignment: .center, spacing: 14) {
            TodoRing(tone: label.tone, done: done)
            VStack(alignment: .leading, spacing: 3) {
                Text(InlineMarkdown.attributed(entry.text))
                    .foregroundStyle(Theme.text)
                    .strikethrough(done, color: Theme.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
                if let text = label.text {
                    Text(text).font(.footnote).foregroundStyle(subtitleColor(label.tone))
                }
                if let note = entry.note {
                    Text(note).font(.footnote).foregroundStyle(Theme.secondary).lineLimit(2)
                }
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
        .frame(minHeight: Theme.minTarget)
        .contentShape(.rect)
        .accessibilityElement(children: .combine)
        .accessibilityLabel([entry.text, label.text].compactMap { $0 }.joined(separator: ", "))
    }

    func subtitleColor(_ tone: DueLabel.Tone) -> Color {
        switch tone {
        case .overdue: Theme.rose
        case .today: Theme.amber
        case .later, .none: Theme.secondary
        }
    }
}

/// Rows stacked in one card with hairline separators.
struct CardList<Item: Identifiable, Row: View>: View {
    let items: [Item]
    @ViewBuilder var row: (Item) -> Row

    var body: some View {
        VStack(spacing: 0) {
            ForEach(Array(items.enumerated()), id: \.element.id) { i, item in
                if i > 0 { Rectangle().fill(Theme.hairline).frame(height: 1).padding(.leading, 52) }
                row(item)
            }
        }
        .card()
    }
}

/// A pinned doc on Today's grid.
struct DocCard: View {
    let doc: DocCardModel
    var now: Date = .now

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Image(systemName: "doc.text")
                .font(.footnote)
                .foregroundStyle(Theme.accent)
                .accessibilityHidden(true)
            Text(doc.title)
                .font(Theme.serif(.headline))
                .foregroundStyle(Theme.text)
                .multilineTextAlignment(.leading)
                .lineLimit(3)
            Spacer(minLength: 0)
            if let sub = doc.subtitle(now: now) {
                Text(sub).font(.caption).foregroundStyle(Theme.secondary).lineLimit(1)
            }
        }
        .padding(14)
        .frame(maxWidth: .infinity, minHeight: 112, alignment: .topLeading)
        .card()
        .contentShape(.rect(cornerRadius: Theme.radius))
        .accessibilityElement(children: .combine)
    }
}

/// `#design`
struct TagChip: View {
    let tag: String
    var selected = false

    var body: some View {
        Text("#\(tag)")
            .font(.footnote.weight(.medium))
            .foregroundStyle(selected ? Theme.ground : Theme.accentActive)
            .padding(.horizontal, 10)
            .padding(.vertical, 5)
            .background(selected ? Theme.accent : Theme.accent.opacity(0.14), in: .capsule)
    }
}

/// A filter chip ("All", "#design") at touch-target height.
struct FilterChip: View {
    let title: String
    let selected: Bool
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Text(title)
                .font(.subheadline.weight(.medium))
                .foregroundStyle(selected ? Theme.ground : Theme.text)
                .padding(.horizontal, 14)
                .frame(minHeight: 34)
                .background(selected ? Theme.accent : Theme.surface2, in: .capsule)
                .overlay(Capsule().strokeBorder(selected ? .clear : Theme.hairline))
                .frame(minHeight: Theme.minTarget)
                .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(selected ? .isSelected : [])
    }
}

/// The sync dot / chip.
struct SyncChip: View {
    let badge: SyncBadge

    var color: Color {
        switch badge.tone {
        case .saved: Theme.green
        case .busy: Theme.accent
        case .offline: Theme.amber
        case .idle: Theme.secondary
        }
    }

    var body: some View {
        HStack(spacing: 6) {
            Circle().fill(color).frame(width: 7, height: 7)
            Text(badge.text).font(.caption.weight(.medium)).foregroundStyle(color)
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 5)
        .background(color.opacity(0.12), in: .capsule)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Sync: \(badge.text)")
    }
}

/// A deliberate empty state inside a card: an icon, a line, and a hint.
struct EmptyCard: View {
    let icon: String
    let title: String
    let hint: String

    var body: some View {
        HStack(alignment: .top, spacing: 14) {
            Image(systemName: icon)
                .font(.title3)
                .foregroundStyle(Theme.accent)
                .frame(width: 28)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 4) {
                Text(title).font(.body.weight(.medium)).foregroundStyle(Theme.text)
                Text(hint).font(.footnote).foregroundStyle(Theme.secondary)
            }
            Spacer(minLength: 0)
        }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .card()
        .accessibilityElement(children: .combine)
    }
}

/// A search field look-alike that opens Search when tapped.
struct SearchFieldButton: View {
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass").foregroundStyle(Theme.secondary)
                Text("Search your library").foregroundStyle(Theme.secondary)
                Spacer()
            }
            .padding(.horizontal, 14)
            .frame(minHeight: Theme.minTarget)
            .background(Theme.surface2, in: .rect(cornerRadius: 12, style: .continuous))
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Search")
        .accessibilityHint("Opens search")
    }
}
