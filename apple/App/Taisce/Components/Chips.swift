import SwiftUI

/// The tones a chip, pill or banner can take: soft fills with the
/// matching ink; neutral is a hairline outline.
enum LapisTone: Hashable, Sendable, CaseIterable {
    case neutral, accent, green, yellow, red, agent

    var ink: Color {
        switch self {
        case .neutral: Color.lapis.ink2
        case .accent: Color.lapis.accent
        case .green: Color.lapis.green
        case .yellow: Color.lapis.yellow
        case .red: Color.lapis.red
        case .agent: Color.lapis.agent
        }
    }

    var soft: Color {
        switch self {
        case .neutral: Color.lapis.card2
        case .accent: Color.lapis.accentSoft
        case .green: Color.lapis.greenSoft
        case .yellow: Color.lapis.yellowSoft
        case .red: Color.lapis.redSoft
        case .agent: Color.lapis.agentSoft
        }
    }

    /// a bare mark: a dot, a bar, an icon
    var mark: Color {
        switch self {
        case .neutral: Color.lapis.ink3
        case .yellow: Color.lapis.yellowMark
        default: ink
        }
    }
}

/// A status, a workspace, a filter: a pill (or a square-ish `tag`) in a
/// tone, with an optional SF Symbol or a coloured dot before the word.
struct StatusChip: View {
    let text: String
    var tone: LapisTone = .neutral
    var symbol: String?
    /// a coloured dot (a workspace's colour)
    var dot: Color?
    /// 6 pt corners instead of a capsule
    var tag = false

    var body: some View {
        HStack(spacing: 5) {
            if let dot {
                Circle().fill(dot).frame(width: 7, height: 7).accessibilityHidden(true)
            }
            if let symbol {
                Image(systemName: symbol).imageScale(.small).fontWeight(.semibold).accessibilityHidden(true)
            }
            Text(text).lineLimit(1)
        }
        .font(.caption.weight(.medium))
        .foregroundStyle(tone.ink)
        .padding(.horizontal, 8)
        .padding(.vertical, 3)
        .background(tone == .neutral ? Color.clear : tone.soft, in: shape)
        .overlay { if tone == .neutral { shape.strokeBorder(Color.lapis.line2, lineWidth: 1) } }
        .accessibilityElement(children: .combine)
    }

    /// a capsule, or 6 pt corners for a tag (a big radius clamps to a capsule)
    private var shape: RoundedRectangle {
        RoundedRectangle(cornerRadius: tag ? Lapis.Radius.sm : 99, style: .continuous)
    }
}

/// `#design`. With an action it is a button at touch-target height.
struct TagChip: View {
    let tag: String
    var action: (() -> Void)?

    var body: some View {
        if let action {
            Button(action: action) { label.frame(minHeight: Lapis.Layout.minTarget).contentShape(.rect) }
                .buttonStyle(.plain)
                .accessibilityLabel("Tag \(tag)")
        } else {
            label.accessibilityLabel("Tag \(tag)")
        }
    }

    private var label: some View {
        Text("#\(tag)")
            .font(.caption2.weight(.medium))
            .foregroundStyle(Color.lapis.ink2)
            .padding(.horizontal, 6)
            .padding(.vertical, 3)
            .background(Color.lapis.card2, in: .rect(cornerRadius: Lapis.Radius.sm, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: Lapis.Radius.sm, style: .continuous).strokeBorder(Color.lapis.line, lineWidth: 1))
    }
}

/// A filter chip ("All", "#design") at touch-target height.
struct FilterChip: View {
    let title: String
    let selected: Bool
    let action: () -> Void

    var body: some View {
        Button {
            Haptics.selection()
            action()
        } label: {
            Text(title)
                .font(.caption.weight(.medium))
                .foregroundStyle(selected ? Color.lapis.onAccent : Color.lapis.ink)
                .padding(.horizontal, 12)
                .frame(minHeight: 28)
                .background(selected ? Color.lapis.accent : Color.lapis.card, in: .capsule)
                .overlay(Capsule().strokeBorder(selected ? .clear : Color.lapis.line2))
                .frame(minHeight: Lapis.Layout.minTarget)
                .contentShape(.rect)
                .animation(Lapis.Motion.fast, value: selected)
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(selected ? .isSelected : [])
    }
}

/// A small count. `.attention` is the yellow badge (Review's pending
/// count), `.accent` an unread count. Nothing for 0 unless `zero`.
struct CountPill: View {
    enum Tone { case neutral, accent, attention }

    let n: Int
    var tone: Tone = .neutral
    var zero = false
    /// read aloud instead of the bare number ("5 to review")
    var label: String?

    var body: some View {
        if n != 0 || zero {
            Text(n > 999 ? "999+" : "\(n)")
                .font(.caption2.weight(.semibold))
                .monospacedDigit()
                .foregroundStyle(ink)
                .padding(.horizontal, 6)
                .frame(minWidth: 18, minHeight: 18)
                .background(fill, in: .capsule)
                .accessibilityLabel(label ?? "\(n)")
        }
    }

    private var ink: Color {
        switch tone {
        case .neutral: Color.lapis.ink2
        case .accent: Color.lapis.onAccent
        case .attention: Color.lapis.onYellow
        }
    }

    private var fill: Color {
        switch tone {
        case .neutral: Color.lapis.card2
        case .accent: Color.lapis.accent
        case .attention: Color.lapis.yellowMark
        }
    }
}

/// When a to-do is due: overdue (red, filled), today (yellow, filled) or
/// upcoming (an outline). Each has its own symbol, so it reads without colour.
struct DeadlinePill: View {
    enum Kind: Hashable, Sendable { case overdue, today, upcoming }

    let kind: Kind
    let text: String

    init(_ kind: Kind, _ text: String) {
        self.kind = kind
        self.text = text
    }

    /// From a to-do's due label; nil when it has no deadline.
    init?(_ label: DueLabel) {
        guard let text = label.text else { return nil }
        switch label.tone {
        case .overdue: self.init(.overdue, text)
        case .today: self.init(.today, text)
        case .later: self.init(.upcoming, text)
        case .none: return nil
        }
    }

    var body: some View {
        HStack(spacing: 4) {
            Image(systemName: symbol).imageScale(.small).fontWeight(.semibold).accessibilityHidden(true)
            Text(text).lineLimit(1).monospacedDigit()
        }
        .font(.caption.weight(.medium))
        .foregroundStyle(ink)
        .padding(.horizontal, 7)
        .padding(.vertical, 2)
        .background(fill, in: .capsule)
        .overlay { if kind == .upcoming { Capsule().strokeBorder(Color.lapis.line2, lineWidth: 1) } }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(spoken)
    }

    private var symbol: String {
        switch kind {
        case .overdue: "exclamationmark.circle"
        case .today: "clock"
        case .upcoming: "calendar"
        }
    }

    private var ink: Color {
        switch kind {
        case .overdue: Color.lapis.red
        case .today: Color.lapis.yellow
        case .upcoming: Color.lapis.ink2
        }
    }

    private var fill: Color {
        switch kind {
        case .overdue: Color.lapis.redSoft
        case .today: Color.lapis.yellowSoft
        case .upcoming: .clear
        }
    }

    private var spoken: String {
        switch kind {
        case .overdue: "Overdue, \(text)"
        case .today: "Due today, \(text)"
        case .upcoming: "Due \(text)"
        }
    }
}

#Preview("Chips · light") {
    LapisPreview(title: "Chips") { chipsGallery }.preferredColorScheme(.light)
}

#Preview("Chips · dark") {
    LapisPreview(title: "Chips") { chipsGallery }.preferredColorScheme(.dark)
}

@MainActor @ViewBuilder private var chipsGallery: some View {
    FlowRow(spacing: 8) {
        ForEach(LapisTone.allCases, id: \.self) { StatusChip(text: "\($0)".capitalized, tone: $0) }
    }
    FlowRow(spacing: 8) {
        StatusChip(text: "Work", dot: Lapis.Token.ws3.color)
        StatusChip(text: "Shared", tone: .accent, symbol: "person.2", tag: true)
        TagChip(tag: "design")
        TagChip(tag: "mobile") {}
    }
    HStack(spacing: 8) {
        CountPill(n: 5)
        CountPill(n: 12, tone: .accent, label: "12 unread")
        CountPill(n: 3, tone: .attention, label: "3 to review")
        CountPill(n: 1200)
    }
    HStack(spacing: 8) {
        FilterChip(title: "All", selected: true) {}
        FilterChip(title: "#design", selected: false) {}
    }
    FlowRow(spacing: 8) {
        DeadlinePill(.overdue, "Yesterday")
        DeadlinePill(.today, "15:00")
        DeadlinePill(.upcoming, "Fri 3 Oct")
    }
}
