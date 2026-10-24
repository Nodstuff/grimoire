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
/// Labels keep one line at ordinary text sizes (shrinking a little before
/// truncating); at accessibility sizes they drop their icon and wrap, so a
/// long label stays whole. `.lapisButtonIcons(false)` shows words only.
struct LapisButtonStyle: ButtonStyle {
    enum Kind: Hashable, Sendable, CaseIterable { case primary, secondary, ghost, destructive, keep, revert }
    /// small 30, regular 36, large 50 (a sheet's main actions: iPhone-Book-Detail)
    enum Size: Hashable, Sendable { case regular, small, large }

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
    @Environment(\.dynamicTypeSize) private var typeSize
    @Environment(\.lapisButtonIcons) private var icons
    @State private var hovering = false

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: size == .large ? 25 : Lapis.Radius.button, style: .continuous)
        let big = typeSize.isAccessibilitySize
        configuration.label
            .font(size == .small ? .footnote.weight(.semibold) : size == .large ? .headline : .subheadline.weight(.semibold))
            .labelStyle(LapisButtonLabelStyle(icon: icons && !big))
            .foregroundStyle(ink)
            .lineLimit(big ? 4 : 1)
            .minimumScaleFactor(big ? 1 : 0.85)
            .multilineTextAlignment(.center)
            .fixedSize(horizontal: false, vertical: big)
            .padding(.vertical, big ? 8 : 0)
            .padding(.horizontal, kind == .ghost || kind == .destructive ? 10 : (size == .small ? 12 : size == .large ? 20 : 16))
            .frame(minHeight: size == .small ? Lapis.Layout.smallButtonHeight : size == .large ? 50 : Lapis.Layout.buttonHeight)
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

/// Icon and title close together, the icon a step smaller; the title alone
/// at accessibility sizes (or where icons are off).
private struct LapisButtonLabelStyle: LabelStyle {
    var icon = true

    func makeBody(configuration: Configuration) -> some View {
        if icon {
            HStack(spacing: 6) {
                configuration.icon.imageScale(.small)
                configuration.title
            }
        } else {
            configuration.title
        }
    }
}

extension EnvironmentValues {
    /// false: Lapis buttons show their words only (the iPad book pane)
    @Entry var lapisButtonIcons = true
}

extension View {
    func lapisButtonIcons(_ on: Bool) -> some View { environment(\.lapisButtonIcons, on) }
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
    Button("Send to an e-reader or another app", systemImage: "paperplane") {}
        .buttonStyle(.lapis(.secondary, fullWidth: true))
        .dynamicTypeSize(.accessibility3)
}
