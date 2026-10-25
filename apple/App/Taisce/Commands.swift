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
            item("New Doc Inside This One", .newDocHere).keyboardShortcut("n", modifiers: [.command, .option])
            item("New Folder", .newFolder).keyboardShortcut("n", modifiers: [.command, .shift])
            Divider()
            item("Open Doc\u{2026}", .open).keyboardShortcut("o")
            item("Add EPUBs\u{2026}", .addBooks).keyboardShortcut("i", modifiers: [.command, .shift])
        }
        // ⌥⌘E (⇧⌘E is inline code in the editor)
        CommandGroup(replacing: .importExport) {
            item("Export as PDF\u{2026}", .exportPDF).keyboardShortcut("e", modifiers: [.command, .option])
        }
        CommandGroup(replacing: .appSettings) {
            item("Settings\u{2026}", .settings).keyboardShortcut(",")
        }
        // ⌘F finds in the doc in front (the system's Find items would claim ⌘F too)
        CommandGroup(replacing: .textEditing) {
            item("Find in Doc\u{2026}", .findInDoc).keyboardShortcut("f")
        }
        CommandMenu("Doc") {
            item("Refresh", .refresh).keyboardShortcut("r")
            Divider()
            item("Share Link\u{2026}", .shareLink)
        }
        // the system's View › Toggle Sidebar keeps ⌃⌘S; ⌘\ is ours, as on the web
        CommandGroup(after: .sidebar) {
            item("Show or Hide Sidebar", .toggleSidebar).keyboardShortcut("\\")
        }
        CommandMenu("Go") {
            item("Back", .back).keyboardShortcut("[")
            item("Forward", .forward).keyboardShortcut("]")
            Divider()
            item("Actions\u{2026}", .actions).keyboardShortcut("k")
            item("Search", .search).keyboardShortcut("f", modifiers: [.command, .shift])
            Divider()
            item("Today", .today)
            item("To-dos", .todos)
            item("Review", .review)
            item("Books", .books)
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
        model.apply(router.perform(command, picker: model.workspacePicker))
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

    /// The bigger window around the same centre the system chose for the
    /// default one (it placed that on screen; growing from its corner ran a
    /// 1280 pt window off a MacBook Air's right edge).
    static func grown(_ frame: CGRect, to size: CGSize) -> CGRect {
        let x = max(0, frame.midX - size.width / 2)
        let y = max(0, frame.midY - size.height / 2)
        return CGRect(x: x, y: y, width: size.width, height: size.height)
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
            // the sidebar carries "◆ Taisce"; the bar shows the screen's own
            // title (a doc's crumbs and save state), not the app's name again
            scene.titlebar?.titleVisibility = .hidden
            // after the scene has finished connecting, or the request is dropped
            DispatchQueue.main.async {
                let frame = scene.effectiveGeometry.systemFrame
                guard let size = MacWindow.size(replacing: frame.size) else { return }
                scene.requestGeometryUpdate(.Mac(systemFrame: MacWindow.grown(frame, to: size)))
            }
        }
    }
}
#endif
