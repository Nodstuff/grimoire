import SwiftUI
import UIKit
import TaisceKit

/// The text view for one block: Backspace at the start, hardware keys and
/// paste come back to its coordinator before UIKit's defaults.
final class BlockTextView: UITextView {
    weak var handler: BlockTextCoordinator?

    override func deleteBackward() {
        if handler?.handleBackspace() == true { return }
        super.deleteBackward()
    }

    override func didMoveToWindow() {
        super.didMoveToWindow()
        handler?.didMoveToWindow()
    }

    override func paste(_ sender: Any?) {
        if handler?.handlePaste() == true { return }
        super.paste(sender)
    }

    override var keyCommands: [UIKeyCommand]? {
        var cmds: [UIKeyCommand] = [
            command("b", .command, #selector(cmdBold), "Bold"),
            command("i", .command, #selector(cmdItalic), "Italic"),
            command("k", .command, #selector(cmdLink), "Link"),
            // ⌘E is the menu bar's Edit / Done; inline code takes ⇧⌘E
            command("e", [.command, .shift], #selector(cmdCode), "Inline Code"),
            command("\t", [], #selector(cmdIndent), "Indent"),
            command("\t", .shift, #selector(cmdOutdent), "Outdent"),
        ]
        if handler?.isCompleting == true {
            cmds += [
                command(UIKeyCommand.inputUpArrow, [], #selector(cmdUp), nil),
                command(UIKeyCommand.inputDownArrow, [], #selector(cmdDown), nil),
                command(UIKeyCommand.inputEscape, [], #selector(cmdEscape), nil),
            ]
        } else {
            if handler?.caretOnFirstLine == true { cmds.append(command(UIKeyCommand.inputUpArrow, [], #selector(cmdPrevBlock), nil)) }
            if handler?.caretOnLastLine == true { cmds.append(command(UIKeyCommand.inputDownArrow, [], #selector(cmdNextBlock), nil)) }
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

    @objc func cmdBold() { handler?.toggle(.bold) }
    @objc func cmdItalic() { handler?.toggle(.italic) }
    @objc func cmdCode() { handler?.toggle(.code) }
    @objc func cmdLink() { handler?.link() }
    @objc func cmdIndent() { handler?.indent(+1) }
    @objc func cmdOutdent() { handler?.indent(-1) }
    @objc func cmdUp() { handler?.completionMove(-1) }
    @objc func cmdDown() { handler?.completionMove(+1) }
    @objc func cmdEscape() { handler?.escape() }
    @objc func cmdPrevBlock() { handler?.moveToNeighbour(-1) }
    @objc func cmdNextBlock() { handler?.moveToNeighbour(+1) }
}

/// One block's `UITextView` in SwiftUI. The view owns the text while it is
/// being typed in; the model reloads it only when `item.revision` moves.
struct BlockEditorView: UIViewRepresentable {
    let item: EditorSession.Item
    let model: EditorModel
    var dynamicType: DynamicTypeSize = .large
    /// bumped by the coordinator when the text's height changed
    var layoutToken = 0
    var onResize: () -> Void = {}

    func makeCoordinator() -> BlockTextCoordinator {
        BlockTextCoordinator(id: item.id, model: model)
    }

    func makeUIView(context: Context) -> BlockTextView {
        let tv = BlockTextView(usingTextLayoutManager: true)
        tv.isScrollEnabled = false
        tv.backgroundColor = .clear
        tv.textContainerInset = UIEdgeInsets(top: 4, left: 0, bottom: 4, right: 0)
        tv.textContainer.lineFragmentPadding = 0
        tv.keyboardDismissMode = .none
        tv.allowsEditingTextAttributes = false
        tv.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        let c = context.coordinator
        tv.delegate = c
        tv.handler = c
        c.textView = tv
        c.onResize = onResize
        // the Mac has no keyboard to ride on: DocEditorView pins the bar on top
        if !EditorChrome.barIsInline { tv.inputAccessoryView = model.formattingBar }
        let tap = UITapGestureRecognizer(target: c, action: #selector(BlockTextCoordinator.tapped(_:)))
        tap.delegate = c
        tap.cancelsTouchesInView = false
        tv.addGestureRecognizer(tap)
        c.load(item, dynamicType: dynamicType)
        model.register(c)
        return tv
    }

    func updateUIView(_ tv: BlockTextView, context: Context) {
        let c = context.coordinator
        c.onResize = onResize
        if c.revision != item.revision || c.dynamicType != dynamicType {
            c.load(item, dynamicType: dynamicType)
        }
        model.register(c)
        c.applyPendingFocus()
    }

    static func dismantleUIView(_ tv: BlockTextView, coordinator: BlockTextCoordinator) {
        coordinator.model?.unregister(coordinator)
    }

    func sizeThatFits(_ proposal: ProposedViewSize, uiView: BlockTextView, context: Context) -> CGSize? {
        let w = proposal.width ?? uiView.bounds.width
        guard w > 0, w.isFinite else { return nil }
        let fit = uiView.sizeThatFits(CGSize(width: w, height: .greatestFiniteMagnitude))
        return CGSize(width: w, height: max(fit.height, 30))
    }
}

/// Delegate and key handling for one block's text view.
@MainActor
final class BlockTextCoordinator: NSObject, UITextViewDelegate, UIGestureRecognizerDelegate {
    let id: BlockID
    private(set) weak var model: EditorModel?
    weak var textView: BlockTextView?
    var onResize: () -> Void = {}
    private(set) var content: EditorBlockContent = .paragraph(AttributedString())
    private(set) var revision = -1
    private(set) var isDraft = false
    private(set) var dynamicType: DynamicTypeSize = .large
    /// marks toggled on the bar with nothing selected: they apply to what's typed next
    private var pendingMarks: InlineMarks?
    private var lastHeight: CGFloat = 0
    private var reloading = false

    init(id: BlockID, model: EditorModel) {
        self.id = id
        self.model = model
    }

    /// The text view's traits at the size SwiftUI asked for (the Mac's
    /// View › Bigger Text sets it on the doc screen; UIKit wouldn't see it).
    private func traits(_ tv: UITextView) -> UITraitCollection {
        tv.traitCollection.modifyingTraits {
            $0.preferredContentSizeCategory = UIContentSizeCategory(dynamicType)
            #if targetEnvironment(macCatalyst)
            // Catalyst ignores the size category: scale by hand
            $0.docScale = DocTextSize.scale(for: dynamicType)
            #endif
        }
    }

    var kind: EditorText.Kind { EditorText.Kind(content) }
    var isCompleting: Bool { model?.completion?.blockID == id }

    // MARK: loading

    func load(_ item: EditorSession.Item, dynamicType: DynamicTypeSize) {
        // never replace text under an open composition (the next update retries)
        guard let tv = textView, tv.markedTextRange == nil else { return }
        content = item.content
        revision = item.revision
        isDraft = item.isDraft
        self.dynamicType = dynamicType
        let caret = tv.isFirstResponder ? currentCaret() : nil
        reloading = true
        tv.attributedText = EditorText.attributed(item.content, traits: traits(tv))
        configure(tv)
        if let caret { setCaret(caret) }
        reloading = false
        updateTyping()
        resizeIfNeeded()
    }

    private func configure(_ tv: BlockTextView) {
        let raw = content.isRaw
        tv.autocorrectionType = raw ? .no : .default
        tv.autocapitalizationType = raw ? .none : .sentences
        tv.smartQuotesType = raw ? .no : .default
        tv.smartDashesType = raw ? .no : .default
        tv.spellCheckingType = raw ? .no : .default
        tv.accessibilityLabel = kind.accessibilityName
        tv.accessibilityIdentifier = "editor.block"
        tv.backgroundColor = raw ? UIColor(Theme.surface2) : .clear
        tv.textContainerInset = raw ? UIEdgeInsets(top: 10, left: 10, bottom: 10, right: 10) : UIEdgeInsets(top: 4, left: 0, bottom: 4, right: 0)
        tv.layer.cornerRadius = raw ? 10 : 0
    }

    func resizeIfNeeded() {
        guard let tv = textView, tv.bounds.width > 0 else { return }
        let h = tv.sizeThatFits(CGSize(width: tv.bounds.width, height: .greatestFiniteMagnitude)).height
        if abs(h - lastHeight) > 0.5 {
            lastHeight = h
            onResize()
        }
    }

    // MARK: carets

    func currentCaret() -> EditorCaret {
        guard let tv = textView else { return .start }
        return EditorText.caret(at: tv.selectedRange.location, in: tv.attributedText, kind: kind)
    }

    func setCaret(_ caret: EditorCaret) {
        guard let tv = textView else { return }
        let loc = EditorText.location(of: caret, in: tv.attributedText, kind: kind)
        tv.selectedRange = NSRange(location: loc, length: 0)
    }

    func focus(_ caret: EditorCaret) {
        guard let tv = textView else { return }
        if !tv.isFirstResponder, tv.window != nil { tv.becomeFirstResponder() }
        setCaret(caret)
        updateTyping()
        model?.caretMoved(self)
    }

    /// a caret was asked for before the view was on screen
    private var wantsKeyboard: EditorCaret?

    /// Take the caret the model asked for, once this view shows the
    /// revision it refers to. Consumed once; the keyboard follows when the
    /// view reaches a window.
    func applyPendingFocus() {
        guard let f = model?.pendingFocus, f.id == id, f.revision <= revision, let tv = textView else { return }
        model?.pendingFocus = nil
        if tv.window == nil {
            setCaret(f.caret)
            wantsKeyboard = f.caret
            return
        }
        focus(f.caret)
    }

    func didMoveToWindow() {
        guard let caret = wantsKeyboard, textView?.window != nil else { return }
        wantsKeyboard = nil
        focus(caret)
    }

    var caretOnFirstLine: Bool {
        guard let tv = textView, let r = tv.selectedTextRange else { return false }
        return tv.caretRect(for: r.start).minY <= tv.textContainerInset.top + 2
    }

    var caretOnLastLine: Bool {
        guard let tv = textView, let r = tv.selectedTextRange else { return false }
        let rect = tv.caretRect(for: r.end)
        return rect.maxY >= tv.contentSize.height - tv.textContainerInset.bottom - 2
    }

    // MARK: UITextViewDelegate

    func textViewDidBeginEditing(_ textView: UITextView) {
        model?.didFocus(self)
    }

    func textViewDidEndEditing(_ textView: UITextView) {
        model?.didBlur(self)
    }

    func textView(_ textView: UITextView, shouldChangeTextIn range: NSRange, replacementText text: String) -> Bool {
        guard let tv = self.textView else { return true }
        if text == "\n" {
            if isCompleting {
                if model?.completion?.results.isEmpty == false { model?.acceptCompletion(); return false }
                model?.dismissCompletion()
            }
            if content.isRaw { return true }
            // Return over a selection replaces it, then splits there
            if range.length > 0 { replace(range, with: "") }
            syncContent()
            model?.returnKey(id, caret: currentCaret())
            return false
        }
        if content.isRaw { return true }
        // wikilinks are atomic: an edit touching one takes all of it,
        // typing inside one goes after it
        let storage = tv.attributedText ?? NSAttributedString()
        if let expanded = Self.expandToWikiLinks(range, in: storage), expanded != range {
            if range.length == 0 {
                let at = expanded.location + expanded.length
                replace(NSRange(location: at, length: 0), with: text)
            } else {
                replace(expanded, with: text)
            }
            return false
        }
        // a multi-line paste into a paragraph-like block: let paste() handle it
        return true
    }

    /// Wikilink runs overlapping `range` (an insertion point strictly
    /// inside one counts), as one range; nil when none.
    static func expandToWikiLinks(_ range: NSRange, in s: NSAttributedString) -> NSRange? {
        guard s.length > 0 else { return nil }
        var lo = range.location, hi = range.location + range.length
        var touched = false
        func runAt(_ i: Int) -> NSRange? {
            guard i >= 0, i < s.length, s.attribute(EditorText.wiki, at: i, effectiveRange: nil) != nil else { return nil }
            var eff = NSRange()
            _ = s.attribute(EditorText.wiki, at: i, longestEffectiveRange: &eff, in: NSRange(location: 0, length: s.length))
            return eff
        }
        if range.length == 0 {
            // strictly inside: the characters on both sides belong to the same run
            if let a = runAt(range.location - 1), let b = runAt(range.location), a == b {
                return a
            }
            return nil
        }
        for i in [range.location, range.location + range.length - 1] {
            if let r = runAt(i) {
                touched = true
                lo = min(lo, r.location)
                hi = max(hi, r.location + r.length)
            }
        }
        return touched ? NSRange(location: lo, length: hi - lo) : nil
    }

    /// Replace characters ourselves (the delegate said no to UIKit's edit).
    func replace(_ range: NSRange, with text: String) {
        guard let tv = textView else { return }
        let m = NSMutableAttributedString(attributedString: tv.attributedText)
        var attrs = tv.typingAttributes
        attrs[EditorText.wiki] = nil
        attrs[EditorText.marker] = nil
        m.replaceCharacters(in: range, with: NSAttributedString(string: text, attributes: attrs))
        tv.attributedText = m
        tv.selectedRange = NSRange(location: range.location + (text as NSString).length, length: 0)
        textViewDidChange(tv)
    }

    func textViewDidChange(_ textView: UITextView) {
        guard !reloading, let tv = self.textView else { return }
        if tv.markedTextRange != nil {
            // mid-composition the storage belongs to the input method:
            // remember the text, but no shortcuts, restyling or reloads
            let typed = content.isRaw ? .raw(tv.text) : EditorText.content(from: tv.attributedText, like: content)
            model?.textChanged(id, content: typed)
            return
        }
        if content.isRaw {
            content = .raw(tv.text)
            model?.textChanged(id, content: content)
            resizeIfNeeded()
            return
        }
        var derived = EditorText.content(from: tv.attributedText, like: content)
        var caret = currentCaret()
        if let (shortcut, at) = EditorCommands.shortcut(derived, caret: caret) {
            derived = shortcut
            caret = at
            render(derived, caret: caret)
        } else if derived.isList, EditorText.attributed(derived, traits: traits(tv)).string != tv.text {
            // markers out of step (a line lost its marker, numbers moved): redraw them
            render(derived, caret: caret)
        } else {
            restyle()
        }
        content = derived
        model?.textChanged(id, content: derived)
        updateCompletion()
        resizeIfNeeded()
    }

    func textViewDidChangeSelection(_ textView: UITextView) {
        guard !reloading, let tv = self.textView, tv.markedTextRange == nil else { return }
        // never inside a list marker
        if kind == .list, tv.selectedRange.length == 0 {
            let loc = tv.selectedRange.location
            for l in EditorText.lines(tv.attributedText) where loc >= l.range.location && loc < l.range.location + l.markerLength {
                tv.selectedRange = NSRange(location: l.range.location + l.markerLength, length: 0)
                break
            }
        }
        // nor inside a wikilink (they're atomic): to its nearer edge
        if tv.selectedRange.length == 0, let run = Self.expandToWikiLinks(tv.selectedRange, in: tv.attributedText) {
            let loc = tv.selectedRange.location
            let edge = loc - run.location < run.location + run.length - loc ? run.location : run.location + run.length
            tv.selectedRange = NSRange(location: edge, length: 0)
        }
        pendingMarks = nil
        updateTyping()
        updateCompletion()
        model?.caretMoved(self)
    }

    /// Redraw the whole block from `c`, keeping the caret.
    func render(_ c: EditorBlockContent, caret: EditorCaret) {
        guard let tv = textView else { return }
        reloading = true
        content = c
        tv.attributedText = EditorText.attributed(c, traits: traits(tv))
        configure(tv)
        setCaret(caret)
        reloading = false
        updateTyping()
    }

    /// Refresh fonts and colours from the model attributes (after typing).
    func restyle() {
        guard let tv = textView else { return }
        let sel = tv.selectedRange
        reloading = true
        let derived = EditorText.content(from: tv.attributedText, like: content)
        let fresh = EditorText.attributed(derived, traits: traits(tv))
        if fresh.string == tv.text, !fresh.isEqual(to: tv.attributedText) {
            tv.textStorage.setAttributedString(fresh)
            tv.selectedRange = sel
        }
        reloading = false
    }

    func syncContent() {
        guard let tv = textView, tv.markedTextRange == nil else { return }
        content = content.isRaw ? .raw(tv.text) : EditorText.content(from: tv.attributedText, like: content)
        model?.textChanged(id, content: content)
    }

    /// What typing inserts next: the marks around the caret (or the bar's
    /// pending ones), never a wikilink or a marker; a link only inside one.
    func updateTyping() {
        guard let tv = textView else { return }
        if content.isRaw {
            tv.typingAttributes = EditorText.look(.raw, marks: [], link: nil, wiki: nil, checked: false, traits: traits(tv))
            return
        }
        let s = tv.attributedText ?? NSAttributedString()
        let loc = tv.selectedRange.location
        var marks = InlineMarks()
        var link: String?
        var prefix: ListPrefix?
        if kind == .list {
            let lines = EditorText.lines(s)
            let c = EditorText.caret(at: loc, in: s, kind: .list)
            prefix = lines[c.line].prefix
        }
        if loc > 0, loc <= s.length {
            let before = s.attributes(at: loc - 1, effectiveRange: nil)
            if before[EditorText.marker] == nil, (s.string as NSString).character(at: loc - 1) != 10 {
                marks = InlineMarks(rawValue: (before[EditorText.marks] as? Int) ?? 0)
                if let l = before[EditorText.link] as? String, loc < s.length, (s.attribute(EditorText.link, at: loc, effectiveRange: nil) as? String) == l {
                    link = l
                }
            }
        }
        if let pendingMarks { marks = pendingMarks }
        var a = EditorText.look(kind, marks: marks, link: link, wiki: nil, checked: prefix?.checkbox == true, traits: traits(tv), prefix: prefix)
        if !marks.isEmpty { a[EditorText.marks] = marks.rawValue }
        if let link { a[EditorText.link] = link }
        tv.typingAttributes = a
        model?.marksChanged(marks)
    }

    // MARK: Backspace

    func handleBackspace() -> Bool {
        guard let tv = textView, tv.selectedRange.length == 0 else { return false }
        let loc = tv.selectedRange.location
        if content.isRaw {
            guard loc == 0, tv.text.isEmpty else { return false }
            model?.backspaceAtStart(id, caret: .start)
            return true
        }
        let s = tv.attributedText ?? NSAttributedString()
        let caret = currentCaret()
        let atStart: Bool
        if kind == .list {
            let l = EditorText.lines(s)[caret.line]
            atStart = loc == l.range.location + l.markerLength
        } else {
            atStart = loc == 0
        }
        if atStart {
            syncContent()
            model?.backspaceAtStart(id, caret: EditorCaret(line: caret.line, offset: 0))
            return true
        }
        // a wikilink goes as a whole
        if loc > 0, s.attribute(EditorText.wiki, at: loc - 1, effectiveRange: nil) != nil {
            var eff = NSRange()
            _ = s.attribute(EditorText.wiki, at: loc - 1, longestEffectiveRange: &eff, in: NSRange(location: 0, length: s.length))
            replace(eff, with: "")
            return true
        }
        return false
    }

    // MARK: paste

    /// Markdown pasted into an empty paragraph becomes blocks; anything
    /// else pastes as plain text in the block's look.
    func handlePaste() -> Bool {
        guard let tv = textView, let text = UIPasteboard.general.string else { return false }
        if content.isRaw { return false }
        if content.isEmpty, case .paragraph = content, text.contains("\n") || text.hasPrefix("#") || text.hasPrefix("- ") || text.hasPrefix("> ") {
            model?.pasteMarkdown(id, markdown: text)
            return true
        }
        var flat = text
        if case .heading = content { flat = flat.replacingOccurrences(of: "\n", with: " ") }
        replace(tv.selectedRange, with: flat)
        return true
    }

    // MARK: formatting

    func toggle(_ mark: InlineMarks) {
        guard let tv = textView, !content.isRaw else { return }
        let r = tv.selectedRange
        if r.length == 0 {
            let current = InlineMarks(rawValue: (tv.typingAttributes[EditorText.marks] as? Int) ?? 0)
            pendingMarks = current.symmetricDifference(mark)
            let saved = pendingMarks
            updateTyping()
            pendingMarks = saved
            return
        }
        let m = NSMutableAttributedString(attributedString: tv.attributedText)
        var all = true
        m.enumerateAttributes(in: r) { a, sub, _ in
            if a[EditorText.marker] != nil || (m.string as NSString).substring(with: sub) == "\n" { return }
            if !InlineMarks(rawValue: (a[EditorText.marks] as? Int) ?? 0).contains(mark) { all = false }
        }
        m.enumerateAttributes(in: r) { a, sub, _ in
            if a[EditorText.marker] != nil { return }
            var marks = InlineMarks(rawValue: (a[EditorText.marks] as? Int) ?? 0)
            if all { marks.remove(mark) } else { marks.insert(mark) }
            if marks.isEmpty { m.removeAttribute(EditorText.marks, range: sub) } else { m.addAttribute(EditorText.marks, value: marks.rawValue, range: sub) }
        }
        reloading = true
        tv.attributedText = m
        tv.selectedRange = r
        reloading = false
        textViewDidChange(tv)
    }

    func link() {
        guard let tv = textView, !content.isRaw else { return }
        if tv.selectedRange.length == 0 {
            insertWikiOpener()
            return
        }
        let r = tv.selectedRange
        model?.askForLink { [weak self] url in
            guard let self, let tv = self.textView, let url, !url.isEmpty else { return }
            let m = NSMutableAttributedString(attributedString: tv.attributedText)
            m.addAttribute(EditorText.link, value: url, range: r)
            self.reloading = true
            tv.attributedText = m
            tv.selectedRange = NSRange(location: r.location + r.length, length: 0)
            self.reloading = false
            self.textViewDidChange(tv)
        }
    }

    func insertWikiOpener() {
        guard let tv = textView, !content.isRaw else { return }
        replace(tv.selectedRange, with: "[[")
    }

    func indent(_ by: Int) {
        guard let tv = textView else { return }
        if content.isRaw {
            if by > 0 { replace(tv.selectedRange, with: "\t") }
            return
        }
        syncContent()
        model?.indent(id, caret: currentCaret(), by: by)
    }

    func escape() {
        if isCompleting { model?.dismissCompletion() } else { textView?.resignFirstResponder() }
    }

    func moveToNeighbour(_ dir: Int) {
        model?.moveFocus(from: id, by: dir)
    }

    // MARK: [[ completion

    func updateCompletion() {
        guard let tv = textView, !content.isRaw, tv.isFirstResponder, tv.selectedRange.length == 0 else {
            if isCompleting { model?.dismissCompletion() }
            return
        }
        let before = EditorText.textBeforeCaret(tv.selectedRange.location, in: tv.attributedText, kind: kind)
        guard let q = WikiCompletion.query(before: before) else {
            if isCompleting { model?.dismissCompletion() }
            return
        }
        var anchor = CGRect.zero
        if let r = tv.selectedTextRange {
            anchor = tv.convert(tv.caretRect(for: r.end), to: nil)
        }
        model?.updateCompletion(self, query: q, anchor: anchor)
    }

    func completionMove(_ by: Int) { model?.moveCompletion(by) }

    /// Replace `[[query` before the caret with the wikilink.
    func insertWikiLink(_ target: String) {
        guard let tv = textView else { return }
        let loc = tv.selectedRange.location
        let before = EditorText.textBeforeCaret(loc, in: tv.attributedText, kind: kind)
        guard let q = WikiCompletion.query(before: before) else { return }
        let len = (q as NSString).length + 2
        let range = NSRange(location: loc - len, length: len)
        let m = NSMutableAttributedString(attributedString: tv.attributedText)
        let marks = InlineMarks(rawValue: (tv.typingAttributes[EditorText.marks] as? Int) ?? 0)
        var a = EditorText.look(kind, marks: marks, link: nil, wiki: target, checked: false, traits: traits(tv))
        a[EditorText.wiki] = target
        if !marks.isEmpty { a[EditorText.marks] = marks.rawValue }
        m.replaceCharacters(in: range, with: NSAttributedString(string: InlineCodec.wikiDisplay(target), attributes: a))
        // a space after it, so typing carries on outside the link
        let after = range.location + (InlineCodec.wikiDisplay(target) as NSString).length
        var plain = tv.typingAttributes
        plain[EditorText.wiki] = nil
        plain[EditorText.link] = nil
        m.insert(NSAttributedString(string: " ", attributes: plain), at: after)
        reloading = true
        tv.attributedText = m
        tv.selectedRange = NSRange(location: after + 1, length: 0)
        reloading = false
        model?.dismissCompletion()
        textViewDidChange(tv)
    }

    // MARK: checkbox taps

    @objc func tapped(_ g: UITapGestureRecognizer) {
        guard let tv = textView, kind == .list, case .list = content else { return }
        let p = g.location(in: tv)
        let lines = EditorText.lines(tv.attributedText)
        guard let pos = tv.closestPosition(to: p) else { return }
        let loc = tv.offset(from: tv.beginningOfDocument, to: pos)
        let caret = EditorText.caret(at: loc, in: tv.attributedText, kind: .list)
        let line = lines[caret.line]
        guard let prefix = line.prefix, let checked = prefix.checkbox else { return }
        // only a tap on the box itself (left of the text)
        let indent = CGFloat(prefix.indent) * EditorText.indentStep
        guard p.x < indent + 24 + tv.textContainerInset.left else { return }
        syncContent()
        model?.setChecked(id, line: caret.line, !checked)
    }

    func gestureRecognizer(_ g: UIGestureRecognizer, shouldRecognizeSimultaneouslyWith other: UIGestureRecognizer) -> Bool { true }
}
