import SwiftUI
import UIKit
import TaisceKit

/// The doc's one text view (TextKit 2). It scrolls itself (so the caret
/// stays in view while typing), carries the doc header above the text and
/// the children list below it as subviews, keeps a reading-width column,
/// and sends Backspace, paste, cut, copy and the hardware keys to its
/// `SurfaceModel` before UIKit's defaults.
final class SurfaceTextView: UITextView {
    weak var model: SurfaceModel?
    /// its own undo stack: typing and the model's edits share it, and no
    /// other screen's actions end up in it
    /// (made on first use: UIKit can ask before Swift's initialisers ran,
    /// `init(usingTextLayoutManager:)` being a factory)
    private var ownUndo: SurfaceUndoManager?
    override var undoManager: UndoManager? {
        if ownUndo == nil {
            let u = SurfaceUndoManager()
            // the model groups: a typing run, or one command
            u.groupsByEvent = false
            ownUndo = u
        }
        ownUndo?.view = self
        return ownUndo
    }

    var header: UIView? {
        didSet { oldValue?.removeFromSuperview(); if let header { addSubview(header) }; setNeedsLayout() }
    }
    var footer: UIView? {
        didSet { oldValue?.removeFromSuperview(); if let footer { addSubview(footer) }; setNeedsLayout() }
    }
    var gutter: CGFloat = 20
    var readingWidth: CGFloat = 720

    override func layoutSubviews() {
        super.layoutSubviews()
        let w = bounds.width
        guard w > 0 else { return }
        let column = max(100, min(w - gutter * 2, readingWidth))
        let side = floor((w - column) / 2)
        var top: CGFloat = 8
        if let header {
            let h = header.sizeThatFits(CGSize(width: column, height: .greatestFiniteMagnitude)).height
            header.frame = CGRect(x: side, y: 4, width: column, height: h)
            top = h + 14
        }
        var bottom: CGFloat = 120
        if let footer {
            let h = footer.sizeThatFits(CGSize(width: column, height: .greatestFiniteMagnitude)).height
            let textBottom = top + (textLayoutManager?.usageBoundsForTextContainer.maxY ?? 0) + 12
            footer.frame = CGRect(x: side, y: textBottom, width: column, height: h)
            bottom = h + 60
        }
        let inset = UIEdgeInsets(top: top, left: side, bottom: bottom, right: side)
        if inset != textContainerInset { textContainerInset = inset }
    }

    /// A tap on the header or the children list is theirs: it doesn't
    /// place the caret (scrolling still starts anywhere).
    override func gestureRecognizerShouldBegin(_ g: UIGestureRecognizer) -> Bool {
        if g !== panGestureRecognizer {
            let p = g.location(in: self)
            if let header, header.frame.contains(p) { return false }
            if let footer, footer.frame.contains(p) { return false }
        }
        return super.gestureRecognizerShouldBegin(g)
    }

    override func deleteBackward() {
        if model?.handleBackspace() == true { return }
        super.deleteBackward()
    }

    override func paste(_ sender: Any?) {
        if model?.handlePaste() == true { return }
        super.paste(sender)
    }

    override func cut(_ sender: Any?) {
        if model?.handleCut() == true { return }
        super.cut(sender)
    }

    override func copy(_ sender: Any?) {
        if model?.handleCopy() == true { return }
        super.copy(sender)
    }

    override var keyCommands: [UIKeyCommand]? {
        guard isEditable else { return nil }
        var cmds: [UIKeyCommand] = [
            command("b", .command, #selector(cmdBold), "Bold"),
            command("i", .command, #selector(cmdItalic), "Italic"),
            command("k", .command, #selector(cmdLink), "Link"),
            command("e", [.command, .shift], #selector(cmdCode), "Inline Code"),
            command("\t", [], #selector(cmdIndent), "Indent"),
            command("\t", .shift, #selector(cmdOutdent), "Outdent"),
        ]
        if model?.completion != nil {
            cmds += [
                command(UIKeyCommand.inputUpArrow, [], #selector(cmdUp), nil),
                command(UIKeyCommand.inputDownArrow, [], #selector(cmdDown), nil),
                command(UIKeyCommand.inputEscape, [], #selector(cmdEscape), nil),
            ]
        } else {
            cmds.append(command(UIKeyCommand.inputEscape, [], #selector(cmdEscape), nil))
        }
        return cmds
    }

    private func command(_ input: String, _ flags: UIKeyModifierFlags, _ action: Selector, _ title: String?) -> UIKeyCommand {
        let c = UIKeyCommand(input: input, modifierFlags: flags, action: action)
        if let title { c.discoverabilityTitle = title }
        c.wantsPriorityOverSystemBehavior = true
        return c
    }

    @objc func cmdBold() { model?.toggle(.bold) }
    @objc func cmdItalic() { model?.toggle(.italic) }
    @objc func cmdCode() { model?.toggle(.code) }
    @objc func cmdLink() { model?.link() }
    @objc func cmdIndent() { model?.indent(+1) }
    @objc func cmdOutdent() { model?.indent(-1) }
    @objc func cmdUp() { model?.moveCompletion(-1) }
    @objc func cmdDown() { model?.moveCompletion(+1) }
    @objc func cmdEscape() {
        if model?.completion != nil { model?.dismissCompletion() } else { resignFirstResponder() }
    }
}

/// The surface's undo stack: an open typing run is closed before an undo
/// or redo (with `groupsByEvent` off, nothing else closes it).
final class SurfaceUndoManager: UndoManager {
    weak var view: SurfaceTextView?

    override func undo() {
        MainActor.assumeIsolated { view?.model?.closeTyping() }
        if groupingLevel > 0 { endUndoGrouping() }
        super.undo()
    }

    override func redo() {
        MainActor.assumeIsolated { view?.model?.closeTyping() }
        if groupingLevel > 0 { endUndoGrouping() }
        super.redo()
    }

    override var canUndo: Bool {
        // an open run has actions even before it is closed
        super.canUndo || (groupingLevel > 0 && MainActor.assumeIsolated { view?.model?.typingRunHasActions ?? false })
    }
}

/// The surface in SwiftUI:the model owns the one text view, so SwiftUI
/// re-rendering never replaces it (or its undo stack, or the caret).
struct DocSurfaceView: UIViewRepresentable {
    let model: SurfaceModel
    var dynamicType: DynamicTypeSize = .large

    func makeUIView(context: Context) -> SurfaceTextView { model.textView }

    func updateUIView(_ tv: SurfaceTextView, context: Context) {
        model.setDynamicType(dynamicType)
    }
}
