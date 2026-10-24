import SwiftUI
import TaisceKit

/// The big serif screen title with an optional line above it.
struct ScreenHeader<Trailing: View>: View {
    let title: String
    var kicker: String?
    @ViewBuilder var trailing: Trailing

    var body: some View {
        HStack(alignment: .bottom) {
            VStack(alignment: .leading, spacing: 2) {
                if let kicker {
                    Text(kicker).font(.subheadline).foregroundStyle(Color.lapis.ink3)
                }
                Text(title)
                    .lapisFont(.screenTitle)
                    .foregroundStyle(Color.lapis.ink)
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
        if done { return Color.lapis.green }
        return switch tone {
        case .overdue: Color.lapis.red
        case .today: Color.lapis.yellowMark
        case .later, .none: Color.lapis.ink3
        }
    }

    var body: some View {
        ZStack {
            Circle().strokeBorder(color, lineWidth: 2)
            if done {
                Image(systemName: "checkmark").font(.system(size: 11, weight: .bold)).foregroundStyle(color)
            }
        }
        .frame(width: 20, height: 20)
        .accessibilityHidden(true)
    }
}

/// One to-do row inside a card list.
struct TodoRow: View {
    let entry: TodoEntry
    var now: Date = .now
    var done = false
    /// the ring as a button (tap to mark done / undo); nil = a plain ring
    var onToggle: (() -> Void)?

    var body: some View {
        let label = DueLabel.make(entry, now: now)
        HStack(alignment: .center, spacing: 14) {
            if let onToggle {
                Button(action: onToggle) {
                    TodoRing(tone: label.tone, done: done)
                        .frame(width: Lapis.Layout.minTarget, height: Lapis.Layout.minTarget)
                        .contentShape(.rect)
                }
                .buttonStyle(.borderless)
                .padding(.horizontal, -12)
                .accessibilityLabel(done ? "Mark not done" : "Mark done")
                .sensoryFeedback(.success, trigger: done) { old, new in new && !old }
            } else {
                TodoRing(tone: label.tone, done: done)
            }
            VStack(alignment: .leading, spacing: 3) {
                Text(InlineMarkdown.attributed(entry.text))
                    .font(.subheadline)
                    .foregroundStyle(Color.lapis.ink)
                    .strikethrough(done, color: Color.lapis.ink3)
                    .opacity(done ? 0.55 : 1)
                    .frame(maxWidth: .infinity, alignment: .leading)
                if let text = label.text {
                    Text(text).font(.caption).foregroundStyle(subtitleColor(label.tone))
                }
                if let note = entry.note {
                    Text(note).font(.caption).foregroundStyle(Color.lapis.ink3).lineLimit(2)
                }
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .frame(minHeight: Lapis.Layout.minTarget)
        .contentShape(.rect)
        .animation(.easeOut(duration: 0.25), value: done)
        .accessibilityElement(children: onToggle == nil ? .combine : .contain)
        .accessibilityLabel([entry.text, label.text].compactMap { $0 }.joined(separator: ", "))
    }

    func subtitleColor(_ tone: DueLabel.Tone) -> Color {
        switch tone {
        case .overdue: Color.lapis.red
        case .today: Color.lapis.yellow
        case .later, .none: Color.lapis.ink3
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
                if i > 0 { Rectangle().fill(Color.lapis.line).frame(height: 1).padding(.leading, 48) }
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
        VStack(alignment: .leading, spacing: 6) {
            Text(doc.title)
                .lapisFont(.cardTitle)
                .foregroundStyle(Color.lapis.ink)
                .multilineTextAlignment(.leading)
                .lineLimit(3)
            Spacer(minLength: 12)
            if let sub = doc.subtitle(now: now) {
                Text(sub).font(.caption2).foregroundStyle(Color.lapis.ink3).lineLimit(1)
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, minHeight: 92, alignment: .topLeading)
        .card()
        .hoverWash()
        .contentShape(.rect(cornerRadius: Lapis.Radius.card))
        .accessibilityElement(children: .combine)
    }
}

/// The sync dot / chip.
struct SyncChip: View {
    let badge: SyncBadge

    var color: Color {
        switch badge.tone {
        case .saved: Color.lapis.green
        case .busy: Color.lapis.accent
        case .offline: Color.lapis.yellow
        case .idle: Color.lapis.ink3
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

/// A search field look-alike that opens Search when tapped.
struct SearchFieldButton: View {
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass").font(.subheadline).foregroundStyle(Color.lapis.ink3)
                Text("Search your library").font(.subheadline).foregroundStyle(Color.lapis.ink3)
                Spacer()
            }
            .padding(.horizontal, 12)
            .frame(minHeight: Lapis.Layout.minTarget)
            .background(Color.lapis.card, in: .rect(cornerRadius: Lapis.Radius.field, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: Lapis.Radius.field, style: .continuous).strokeBorder(Color.lapis.line2, lineWidth: 1))
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Search")
        .accessibilityHint("Opens search")
    }
}

/// A round icon button (the Today gear, the To-dos "+").
struct CircleIconButton: View {
    let systemImage: String
    let label: String
    var filled = false
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Image(systemName: systemImage)
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(filled ? Color.lapis.onAccent : Color.lapis.ink3)
                .frame(width: 34, height: 34)
                .background(filled ? Color.lapis.accent : Color.lapis.card, in: .circle)
                .overlay(Circle().strokeBorder(filled ? .clear : Color.lapis.line2, lineWidth: 1))
                .frame(width: Lapis.Layout.minTarget, height: Lapis.Layout.minTarget)
                .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .accessibilityLabel(label)
    }
}

/// A one-line prompt card with an action ("Get a nudge when things are due · Turn on").
struct PromptCard: View {
    let icon: String
    let text: String
    let action: String
    let onAction: () -> Void

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: icon).font(.subheadline).foregroundStyle(Color.lapis.accent).accessibilityHidden(true)
            Text(text).font(.subheadline).foregroundStyle(Color.lapis.ink).lineLimit(2)
            Spacer(minLength: 8)
            Button(action, action: onAction)
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(Color.lapis.accent)
                .frame(minHeight: Lapis.Layout.minTarget)
        }
        .padding(.horizontal, 14)
        .card()
    }
}

/// Children left to right, wrapping onto new lines (tag chips + meta).
struct FlowRow: Layout {
    var spacing: CGFloat = 6

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let rows = arrange(proposal.width ?? .infinity, subviews)
        let width = rows.map { $0.width }.max() ?? 0
        let height = rows.reduce(0) { $0 + $1.height } + spacing * CGFloat(max(rows.count - 1, 0))
        return CGSize(width: width, height: height)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        var y = bounds.minY
        for row in arrange(bounds.width, subviews) {
            var x = bounds.minX
            for i in row.items {
                let size = subviews[i].sizeThatFits(.unspecified)
                subviews[i].place(at: CGPoint(x: x, y: y + (row.height - size.height) / 2), proposal: ProposedViewSize(size))
                x += size.width + spacing
            }
            y += row.height + spacing
        }
    }

    private struct Row { var items: [Int] = []; var width: CGFloat = 0; var height: CGFloat = 0 }

    private func arrange(_ maxWidth: CGFloat, _ subviews: Subviews) -> [Row] {
        var rows: [Row] = [Row()]
        for i in subviews.indices {
            let size = subviews[i].sizeThatFits(.unspecified)
            let needed = rows[rows.count - 1].items.isEmpty ? size.width : rows[rows.count - 1].width + spacing + size.width
            if needed > maxWidth, !rows[rows.count - 1].items.isEmpty {
                rows.append(Row())
            }
            let r = rows.count - 1
            rows[r].width = rows[r].items.isEmpty ? size.width : rows[r].width + spacing + size.width
            rows[r].height = max(rows[r].height, size.height)
            rows[r].items.append(i)
        }
        return rows.filter { !$0.items.isEmpty }
    }
}
