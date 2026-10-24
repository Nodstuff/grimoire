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
    /// inline change cards (flagged / parked review items), by the block
    /// each covers: subviews placed over the block's text
    var cardViews: [BlockID: UIView] = [:]
    /// the text or the review items changed: the cards are placed again
    var cardsDirty = false
    /// the text column's width (the reading width, or less)
    private(set) var columnWidth: CGFloat = 0
    private var lastCardWidth: CGFloat = 0

    /// the header or footer's content changed: measure them again
    var chromeDirty = true
    /// the text changed: the footer's place below it is due again
    var footerPlacementDirty = true
    private var headerSize: CGSize?
    private var footerSize: CGSize?
    private var footerY: CGFloat = 0

    /// Header and footer sizes are cached (measured again only when their
    /// content or the column width changes, never per scroll frame); the
    /// footer goes below the fully laid-out text, placed again only when
    /// the text changed and it is near the screen. A header that changes
    /// height moves the content by the same amount, so the text under the
    /// caret stays where it is.
    override func layoutSubviews() {
        super.layoutSubviews()
        let w = bounds.width
        guard w > 0 else { return }
        let column = max(100, min(w - gutter * 2, readingWidth))
        let side = floor((w - column) / 2)
        columnWidth = column
        func measure(_ v: UIView, _ cached: CGSize?) -> CGSize {
            if let cached, cached.width == column, !chromeDirty { return cached }
            return CGSize(width: column, height: ceil(v.sizeThatFits(CGSize(width: column, height: .greatestFiniteMagnitude)).height))
        }
        var top: CGFloat = 8
        if let header {
            let size = measure(header, headerSize)
            headerSize = size
            header.frame = CGRect(x: side, y: 4, width: column, height: size.height)
            top = size.height + 14
        } else {
            headerSize = nil
        }
        var bottom: CGFloat = 120
        if let footer {
            let size = measure(footer, footerSize)
            if footerSize?.width != size.width { footerPlacementDirty = true }
            footerSize = size
            // near the end, or the text shrank under a stale placement
            let near = footerY == 0 || contentOffset.y + bounds.height * 2 >= footerY || footerY > contentSize.height
            if footerPlacementDirty || top != textContainerInset.top, near, let tlm = textLayoutManager {
                tlm.ensureLayout(for: tlm.documentRange)
                footerY = top + tlm.usageBoundsForTextContainer.maxY + 12
                footerPlacementDirty = false
            }
            footer.frame = CGRect(x: side, y: footerY, width: column, height: size.height)
            bottom = size.height + 60
        } else {
            footerSize = nil
        }
        chromeDirty = false
        let inset = UIEdgeInsets(top: top, left: side, bottom: bottom, right: side)
        if inset != textContainerInset {
            let dy = inset.top - textContainerInset.top
            let atTop = contentOffset.y <= -adjustedContentInset.top + 1
            textContainerInset = inset
            // the text stays put (at the very top, the header shows whole)
            if dy != 0, !atTop { contentOffset.y += dy }
            cardsDirty = true
        }
        if !cardViews.isEmpty || cardsDirty {
            if cardsDirty || lastCardWidth != column {
                lastCardWidth = column
                cardsDirty = false
                model?.layoutCards()
            }
        }
    }

    /// A tap on the header or the children list is theirs: it doesn't
    /// place the caret (scrolling still starts anywhere).
    override func gestureRecognizerShouldBegin(_ g: UIGestureRecognizer) -> Bool {
        if g !== panGestureRecognizer {
            let p = g.location(in: self)
            if let header, header.frame.contains(p) { return false }
            if let footer, footer.frame.contains(p) { return false }
            // a change card's buttons and words are its own
            if cardViews.values.contains(where: { $0.frame.contains(p) }) { return false }
            // a tap on a link or a checkbox is the surface's own (follow /
            // tick): the text view's taps don't place the caret or raise
            // the keyboard there. Long-press and the keys still edit it.
            if g is UITapGestureRecognizer, let model, g !== model.ownTap, model.hit(at: p) != nil { return false }
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
