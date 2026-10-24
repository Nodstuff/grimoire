import SwiftUI

/// Lapis buttons (Lapis-System): `.buttonStyle(.lapis(.primary))`.
///
/// - primary: the one main action on a surface (lapis fill)
/// - secondary: everything else (card fill, firm hairline)
/// - ghost: quiet, inline (accent text, no fill)
/// - destructive: "Delete…" (red text, no fill; confirm before acting)
/// - keep: accept a flagged or parked change (green soft fill)
/// - revert: undo a flagged change (the secondary look)
///
/// The visible button is 36 pt (30 small) and its hit target always 44.
struct LapisButtonStyle: ButtonStyle {
    enum Kind: Hashable, Sendable, CaseIterable { case primary, secondary, ghost, destructive, keep, revert }
    enum Size: Hashable, Sendable { case regular, small }

    var kind: Kind = .secondary
    var size: Size = .regular
    var fullWidth = false

    func makeBody(configuration: Configuration) -> some View {
        LapisButtonBody(configuration: configuration, kind: kind, size: size, fullWidth: fullWidth)
    }
}

extension ButtonStyle where Self == LapisButtonStyle {
    static func lapis(_ kind: LapisButtonStyle.Kind = .secondary, size: LapisButtonStyle.Size = .regular, fullWidth: Bool = false) -> LapisButtonStyle {
        LapisButtonStyle(kind: kind, size: size, fullWidth: fullWidth)
    }
}

private struct LapisButtonBody: View {
    let configuration: ButtonStyleConfiguration
    let kind: LapisButtonStyle.Kind
    let size: LapisButtonStyle.Size
    let fullWidth: Bool
    @Environment(\.isEnabled) private var isEnabled
    @State private var hovering = false

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: Lapis.Radius.button, style: .continuous)
        configuration.label
            .font(size == .small ? .footnote.weight(.semibold) : .subheadline.weight(.semibold))
            .labelStyle(LapisButtonLabelStyle())
            .foregroundStyle(ink)
            .lineLimit(1)
            .padding(.horizontal, kind == .ghost || kind == .destructive ? 10 : (size == .small ? 12 : 16))
            .frame(minHeight: size == .small ? Lapis.Layout.smallButtonHeight : Lapis.Layout.buttonHeight)
            .frame(maxWidth: fullWidth ? .infinity : nil)
            .background(fill, in: shape)
            .overlay { if bordered { shape.strokeBorder(Color.lapis.line2, lineWidth: 1) } }
            .overlay { shape.fill(Color.lapis.ink.opacity(hovering && isEnabled ? 0.05 : 0)).allowsHitTesting(false) }
            .opacity(isEnabled ? (configuration.isPressed ? 0.72 : 1) : 0.45)
            .scaleEffect(configuration.isPressed ? 0.98 : 1)
            .frame(minHeight: Lapis.Layout.minTarget)
            .contentShape(.rect)
            .onHover { hovering = $0 }
            .animation(Lapis.Motion.fast, value: configuration.isPressed)
    }

    private var bordered: Bool { kind == .secondary || kind == .revert }

    private var ink: Color {
        switch kind {
        case .primary: Color.lapis.onAccent
        case .secondary, .revert: Color.lapis.ink
        case .ghost: Color.lapis.accent
        case .destructive: Color.lapis.red
        case .keep: Color.lapis.green
        }
    }

    private var fill: Color {
        switch kind {
        case .primary: Color.lapis.accent
        case .secondary, .revert: Color.lapis.card
        case .ghost, .destructive: .clear
        case .keep: Color.lapis.greenSoft
        }
    }
}

/// Icon and title close together, the icon a step smaller.
private struct LapisButtonLabelStyle: LabelStyle {
    func makeBody(configuration: Configuration) -> some View {
        HStack(spacing: 6) {
            configuration.icon.imageScale(.small)
            configuration.title
        }
    }
}

#Preview("Buttons · light") {
    LapisPreview(title: "Buttons") { buttonsGallery }.preferredColorScheme(.light)
}

#Preview("Buttons · dark") {
    LapisPreview(title: "Buttons") { buttonsGallery }.preferredColorScheme(.dark)
}

@MainActor @ViewBuilder private var buttonsGallery: some View {
    FlowRow(spacing: 8) {
        Button("New doc") {}.buttonStyle(.lapis(.primary))
        Button("Capture") {}.buttonStyle(.lapis(.secondary))
        Button("Keep", systemImage: "checkmark") {}.buttonStyle(.lapis(.keep))
        Button("Revert", systemImage: "arrow.uturn.backward") {}.buttonStyle(.lapis(.revert))
        Button("Delete…", role: .destructive) {}.buttonStyle(.lapis(.destructive))
        Button("Open") {}.buttonStyle(.lapis(.ghost))
    }
    FlowRow(spacing: 8) {
        Button("Small") {}.buttonStyle(.lapis(.primary, size: .small))
        Button("Small") {}.buttonStyle(.lapis(.secondary, size: .small))
        Button("Disabled") {}.buttonStyle(.lapis(.primary)).disabled(true)
    }
    Button("Sign in with passkey", systemImage: "person.badge.key.fill") {}.buttonStyle(.lapis(.primary, fullWidth: true))
}
