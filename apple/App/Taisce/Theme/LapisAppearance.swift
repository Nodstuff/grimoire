import SwiftUI
import UIKit

/// Settings › Appearance: System (follow the device), Light or Dark.
/// A device preference, not the person's: it stays across sign-out
/// (`UserSettings.appearanceKey`, deliberately not in `UserSettings.keys`).
enum LapisAppearance: String, CaseIterable, Identifiable, Sendable {
    case system, light, dark

    var id: String { rawValue }

    var title: String {
        switch self {
        case .system: "System"
        case .light: "Light"
        case .dark: "Dark"
        }
    }

    var symbol: String {
        switch self {
        case .system: "circle.lefthalf.filled"
        case .light: "sun.max"
        case .dark: "moon"
        }
    }

    var interfaceStyle: UIUserInterfaceStyle {
        switch self {
        case .system: .unspecified
        case .light: .light
        case .dark: .dark
        }
    }

    init(stored: String?) {
        self = stored.flatMap(LapisAppearance.init(rawValue:)) ?? .system
    }
}

extension View {
    /// Applies Settings › Appearance to the window this view is in (and so
    /// to its sheets, popovers and UIKit views, the editor included). A
    /// window override rather than `preferredColorScheme`: it reverts
    /// cleanly to System and holds on Mac Catalyst.
    func lapisAppearance() -> some View {
        modifier(LapisAppearanceModifier())
    }
}

private struct LapisAppearanceModifier: ViewModifier {
    @AppStorage(UserSettings.appearanceKey) private var stored = LapisAppearance.system.rawValue

    func body(content: Content) -> some View {
        content.background(WindowStyleSetter(style: LapisAppearance(stored: stored).interfaceStyle))
    }
}

/// Sets `overrideUserInterfaceStyle` on the hosting window.
private struct WindowStyleSetter: UIViewRepresentable {
    let style: UIUserInterfaceStyle

    func makeUIView(context: Context) -> Probe { Probe(style: style) }

    func updateUIView(_ view: Probe, context: Context) {
        view.style = style
        view.apply()
    }

    final class Probe: UIView {
        var style: UIUserInterfaceStyle

        init(style: UIUserInterfaceStyle) {
            self.style = style
            super.init(frame: .zero)
            isUserInteractionEnabled = false
            isAccessibilityElement = false
        }

        @available(*, unavailable)
        required init?(coder: NSCoder) { fatalError() }

        override func didMoveToWindow() {
            super.didMoveToWindow()
            apply()
        }

        func apply() {
            guard let window, window.overrideUserInterfaceStyle != style else { return }
            UIView.transition(with: window, duration: 0.2, options: [.transitionCrossDissolve, .allowUserInteraction]) {
                window.overrideUserInterfaceStyle = self.style
            }
        }
    }
}

/// UIKit chrome in Lapis: navigation bar titles in Newsreader, the
/// window tint. Called once at launch.
enum LapisChrome {
    @MainActor static func install() {
        let nav = UINavigationBar.appearance()
        nav.largeTitleTextAttributes = [
            .font: UIFontMetrics(forTextStyle: .largeTitle).scaledFont(for: Newsreader.uiFont(size: 34, weight: .medium)),
            .foregroundColor: UIColor.lapis.ink,
        ]
        nav.titleTextAttributes = [.foregroundColor: UIColor.lapis.ink]
        nav.tintColor = UIColor.lapis.accent
        UITextField.appearance().tintColor = UIColor.lapis.accent
        UITextView.appearance().tintColor = UIColor.lapis.accent
    }
}
