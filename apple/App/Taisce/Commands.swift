import SwiftUI
import TaisceKit
import UIKit

extension FocusedValues {
    /// The front window's router, for the menu bar.
    @Entry var router: Router?
}

/// The menu bar (and an iPad's ⌘-hold overlay). Each item asks the front
/// window's router; what the router can't do (sync, workspaces) lands on
/// the model.
struct TaisceCommands: Commands {
    let model: AppModel
    @FocusedValue(\.router) private var router
    @AppStorage(DocTextSize.key) private var textSize = DocTextSize.actual

    var body: some Commands {
        #if targetEnvironment(macCatalyst)
        CommandGroup(before: .toolbar) {
            Button("Bigger Text") { textSize = DocTextSize.bigger(textSize) }
                .keyboardShortcut("+")
                .disabled(!DocTextSize.canGrow(textSize))
            Button("Smaller Text") { textSize = DocTextSize.smaller(textSize) }
                .keyboardShortcut("-")
                .disabled(!DocTextSize.canShrink(textSize))
            Button("Actual Size") { textSize = DocTextSize.actual }
                .keyboardShortcut("0")
                .disabled(textSize == DocTextSize.actual)
            Divider()
        }
        #endif
        CommandGroup(replacing: .newItem) {
            item("New Doc", .newDoc).keyboardShortcut("n")
        }
        // ⌥⌘E: ⌘E is Edit Doc and ⇧⌘E inline code in the editor
        CommandGroup(replacing: .importExport) {
            item("Export as PDF\u{2026}", .exportPDF).keyboardShortcut("e", modifiers: [.command, .option])
        }
        CommandGroup(replacing: .appSettings) {
            item("Settings\u{2026}", .settings).keyboardShortcut(",")
        }
        CommandMenu("Doc") {
            item(router?.editingDoc != nil ? "Done Editing" : "Edit Doc", .toggleEdit).keyboardShortcut("e")
            item("Refresh", .refresh).keyboardShortcut("r")
            Divider()
            item("Share Link\u{2026}", .shareLink)
        }
        CommandMenu("Go") {
            item("Search", .search).keyboardShortcut("f")
            item("Today", .today)
            item("To-dos", .todos)
            if model.hasWorkspaces {
                Divider()
                let picker = model.workspacePicker
                ForEach(Array(picker.options.prefix(9).enumerated()), id: \.offset) { i, scope in
                    item(picker.menuTitle(scope), .workspace(i + 1))
                        .keyboardShortcut(KeyEquivalent(Character(String(i + 1))))
                }
            }
        }
    }

    private var signedIn: Bool { !model.isCheckingAuth && !model.needsSignIn }

    private func item(_ title: String, _ command: AppCommand) -> some View {
        Button(title) { run(command) }
            .disabled(!(router?.canPerform(
                command, signedIn: signedIn, picker: model.workspacePicker,
                canEdit: { model.access(for: $0).canEdit }, canCreate: model.currentAccess.canCreate,
                shares: model.shareLinks.isAvailable
            ) ?? false))
    }

    private func run(_ command: AppCommand) {
        guard let router else { return }
        switch router.perform(command, picker: model.workspacePicker) {
        case .none: break
        case .refresh: Task { try? await model.sync?.catchUp() }
        case .selectWorkspace(let scope): model.selectWorkspace(scope)
        }
    }
}

#if targetEnvironment(macCatalyst)
/// The Mac window's size: a floor under which the split view stops
/// working, and a first-launch size. macOS restores the user's size after
/// that; a window still at Catalyst's untouched 1024×768 gets ours.
enum MacWindow {
    static let minimum = CGSize(width: 820, height: 560)
    static let initial = CGSize(width: 1280, height: 860)
    static let catalystDefault = CGSize(width: 1024, height: 768)

    static func size(replacing current: CGSize) -> CGSize? {
        current == catalystDefault ? initial : nil
    }
}

/// Finds the window scene from inside SwiftUI and sizes it.
struct MacWindowSetup: UIViewRepresentable {
    func makeUIView(context: Context) -> SceneProbe { SceneProbe() }
    func updateUIView(_ view: SceneProbe, context: Context) {}

    final class SceneProbe: UIView {
        override func didMoveToWindow() {
            super.didMoveToWindow()
            guard let scene = window?.windowScene else { return }
            scene.sizeRestrictions?.minimumSize = MacWindow.minimum
            // after the scene has finished connecting, or the request is dropped
            DispatchQueue.main.async {
                let frame = scene.effectiveGeometry.systemFrame
                guard let size = MacWindow.size(replacing: frame.size) else { return }
                scene.requestGeometryUpdate(.Mac(systemFrame: CGRect(origin: frame.origin, size: size)))
            }
        }
    }
}
#endif
