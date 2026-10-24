import SwiftUI

/// A small uppercase section label ("DUE", "PINNED").
struct SectionLabel: View {
    let title: String
    var color: Color = Color.lapis.ink3
    var trailing: AnyView?

    init(_ title: String, color: Color = Color.lapis.ink3, trailing: AnyView? = nil) {
        self.title = title
        self.color = color
        self.trailing = trailing
    }

    var body: some View {
        HStack(alignment: .firstTextBaseline) {
            Text(title.uppercased())
                .lapisFont(.label)
                .foregroundStyle(color)
                .accessibilityLabel(title)
                .accessibilityAddTraits(.isHeader)
            Spacer()
            trailing
        }
        .padding(.horizontal, 4)
    }
}

/// A titled section: the label, then its content (usually a card).
struct LapisSection<Content: View, Trailing: View>: View {
    let title: String
    @ViewBuilder var trailing: Trailing
    @ViewBuilder var content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .firstTextBaseline) {
                SectionLabel(title)
                trailing
            }
            content
        }
    }
}

extension LapisSection where Trailing == EmptyView {
    init(_ title: String, @ViewBuilder content: () -> Content) {
        self.title = title
        self.trailing = EmptyView()
        self.content = content()
    }
}

/// Content in a Lapis card with the standard padding.
/// `LapisCard { … }`, `LapisCard(inset: true) { … }` for a flat card2 well.
struct LapisCard<Content: View>: View {
    var inset = false
    var padding: CGFloat = Lapis.Space.s4
    @ViewBuilder var content: Content

    var body: some View {
        content
            .padding(padding)
            .frame(maxWidth: .infinity, alignment: .leading)
            .card(inset ? Color.lapis.card2 : Color.lapis.card, elevated: !inset)
    }
}

/// A deliberate empty state: a symbol in a soft disc, a serif line, a
/// hint, and optionally the one action that fills it.
struct EmptyState: View {
    let symbol: String
    let title: String
    var message: String?
    var actionTitle: String?
    var action: (() -> Void)?

    var body: some View {
        VStack(spacing: 14) {
            Image(systemName: symbol)
                .font(.title2.weight(.medium))
                .foregroundStyle(Color.lapis.accent)
                .frame(width: 56, height: 56)
                .background(Color.lapis.accentSoft, in: .circle)
                .accessibilityHidden(true)
            VStack(spacing: 6) {
                Text(title)
                    .lapisFont(.docHeading2)
                    .foregroundStyle(Color.lapis.ink)
                if let message {
                    Text(message)
                        .font(.subheadline)
                        .foregroundStyle(Color.lapis.ink3)
                }
            }
            .multilineTextAlignment(.center)
            .frame(maxWidth: 340)
            .accessibilityElement(children: .combine)
            if let actionTitle, let action {
                Button(actionTitle, action: action).buttonStyle(.lapis(.primary))
                    .padding(.top, 4)
            }
        }
        .padding(.vertical, 32)
        .padding(.horizontal, 20)
        .frame(maxWidth: .infinity)
    }
}

/// A one-line empty state inside a card: an icon, a line, and a hint.
struct EmptyCard: View {
    let icon: String
    let title: String
    let hint: String

    var body: some View {
        HStack(alignment: .top, spacing: 14) {
            Image(systemName: icon)
                .font(.body)
                .foregroundStyle(Color.lapis.accent)
                .frame(width: 32, height: 32)
                .background(Color.lapis.accentSoft, in: .rect(cornerRadius: Lapis.Radius.md, style: .continuous))
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 4) {
                Text(title).font(.subheadline.weight(.semibold)).foregroundStyle(Color.lapis.ink)
                Text(hint).font(.caption).foregroundStyle(Color.lapis.ink3)
            }
            Spacer(minLength: 0)
        }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .card()
        .accessibilityElement(children: .combine)
    }
}

#Preview("Cards · light") {
    LapisPreview { cardsGallery }.preferredColorScheme(.light)
}

#Preview("Cards · dark") {
    LapisPreview { cardsGallery }.preferredColorScheme(.dark)
}

@MainActor @ViewBuilder private var cardsGallery: some View {
    LapisSection("Pinned") {
        LapisCard {
            Text("Roadmap").lapisFont(.cardTitle).foregroundStyle(Color.lapis.ink)
            Text("Edited 2 h ago").font(.caption).foregroundStyle(Color.lapis.ink3)
        }
    }
    LapisSection("Inset") {
        LapisCard(inset: true) { Text("A flat well for fields and footers").font(.subheadline).foregroundStyle(Color.lapis.ink2) }
    }
    EmptyCard(icon: "pin", title: "No pinned docs", hint: "Long-press a doc in Library to pin it here.")
    EmptyState(symbol: "books.vertical", title: "No books yet", message: "Books you add on the web or send to Taisce appear here.", actionTitle: "Add a book") {}
        .card()
}
