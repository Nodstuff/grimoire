import UIKit
import TaisceKit

/// The text view's delegate: which edits UIKit makes itself (typing in a
/// line) and which the model makes (anything that splits or joins lines,
/// touches a non-text block or a wikilink), so block ids stay right.
extension SurfaceModel: UITextViewDelegate, UIGestureRecognizerDelegate {
    func textView(_ textView: UITextView, shouldChangeTextIn range: NSRange, replacementText text: String) -> Bool {
        guard editable else { return false }
        lastTyped = nil
        typedRange = nil
        pendingTyping = nil
        // mid-composition the input method owns the text
        if textView.markedTextRange != nil { return true }
        let s = self.textView.textStorage
        if text == "\n" {
            if let c = completion {
                if !c.results.isEmpty { acceptCompletion(); return false }
                dismissCompletion()
            }
            handleReturn(range)
            return false
        }
        // a deletion that would leave half a non-text block takes all of it
        if text.isEmpty, range.length > 0 {
            let r = Surface.expandDeletion(range, in: s)
            if r != range {
                perform(Surface.replace(s, r, with: ""))
                return false
            }
        }
        // wikilinks are atomic: an edit touching one takes all of it,
        // typing inside one goes after it
        if let expanded = Surface.expandToWikiLinks(range, in: s), expanded != range {
            if range.length == 0 {
                perform(Surface.replace(s, NSRange(location: expanded.location + expanded.length, length: 0), with: text, typing: plainTyping()))
            } else {
                perform(Surface.replace(s, expanded, with: text, typing: plainTyping()))
            }
            return false
        }
        let ns = s.mutableString
        let crosses = range.length > 0 && (ns.range(of: "\n", options: .literal, range: range).location != NSNotFound
            || ns.range(of: Surface.attachment, options: .literal, range: range).location != NSNotFound)
        if crosses || text.contains("\n") || text.contains("\r") {
            perform(Surface.replace(s, range, with: text, typing: textView.typingAttributes))
            return false
        }
        // typing beside a non-text block: a new paragraph before or after it
        let ls = Surface.lines(s)
        if !ls.isEmpty, !text.isEmpty {
            let l = ls[Surface.lineIndex(ls, at: min(range.location, s.length - 1))]
            if l.style == .raw {
                let id = Surface.newID()
                let line = Surface.render(.paragraph(AttributedString(text)), id: id)
                let at = range.location <= l.range.location ? l.range.location : l.end
                perform(Surface.Edit(range: NSRange(location: at, length: 0), replacement: line,
                                     selection: NSRange(location: at + (text as NSString).length, length: 0)))
                return false
            }
        }
        // ordinary typing in a line: UIKit inserts it with the line's attributes
        textView.typingAttributes = SurfaceLook.typing(s, at: min(range.location, max(0, s.length - 1)), pending: pendingMarks, ctx: lookContext)
        lastTyped = text
        typedRange = NSRange(location: range.location, length: (text as NSString).length)
        pendingTyping = (range, s.attributedSubstring(from: range), s.length, textView.selectedRange)
        return true
    }

    /// Typing attributes with no wikilink or link (text typed beside one).
    private func plainTyping() -> [NSAttributedString.Key: Any] {
        var a = textView.typingAttributes
        a[Surface.wiki] = nil
        a[Surface.link] = nil
        return a
    }

    func textViewDidChange(_ textView: UITextView) {
        didChangeText()
        if textView.markedTextRange == nil { compositionEnded() }
    }

    func textViewDidChangeSelection(_ textView: UITextView) {
        guard !reloading, textView.markedTextRange == nil else { return }
        let s = self.textView.textStorage
        guard s.length > 0 else { return }
        var sel = textView.selectedRange
        if editable {
            // never past the final line break
            if sel.location + sel.length > s.length - 1 {
                let loc = min(sel.location, s.length - 1)
                sel = NSRange(location: loc, length: max(0, s.length - 1 - loc))
            }
            // neither end inside a list marker
            if let l = Surface.line(at: sel.location, in: s), l.markerLength > 0,
               sel.location >= l.range.location, sel.location < l.range.location + l.markerLength,
               sel.length == 0 || sel.location > l.range.location {
                let end = sel.location + sel.length
                sel.location = l.range.location + l.markerLength
                sel.length = max(0, end - sel.location)
            }
            if sel.length > 0, let l = Surface.line(at: sel.location + sel.length, in: s), l.markerLength > 0 {
                let end = sel.location + sel.length
                if end > l.range.location, end < l.range.location + l.markerLength {
                    sel.length = l.range.location + l.markerLength - sel.location
                }
            }
            if sel.length == 0 {
                // nor inside a wikilink: to its nearer edge
                if let run = Surface.expandToWikiLinks(sel, in: s) {
                    let loc = sel.location
                    sel.location = loc - run.location < run.location + run.length - loc ? run.location : run.location + run.length
                }
            }
            if sel != textView.selectedRange {
                reloading = true
                textView.selectedRange = sel
                reloading = false
            }
        }
        pendingMarks = nil
        // the caret moved on its own (not by a keystroke): a new undo step
        if pendingTyping == nil, typedRange == nil { closeTyping() }
        updateTyping()
        updateCompletion()
    }

    func textViewDidEndEditing(_ textView: UITextView) {
        if completion != nil { dismissCompletion() }
        Task { await flush() }
    }

    // Read-only text: a link tap follows it here (wikilinks in the app).
    func textView(_ textView: UITextView, primaryActionFor textItem: UITextItem, defaultAction: UIAction) -> UIAction? {
        guard case let .link(url) = textItem.content else { return defaultAction }
        return UIAction { [weak self] _ in self?.follow(url) }
    }

    // The edit menu (long-press on iOS, right-click on the Mac): Open Link.
    func textView(_ textView: UITextView, editMenuForTextIn range: NSRange, suggestedActions: [UIMenuElement]) -> UIMenu? {
        let at = range.length > 0 ? range.location : max(0, range.location - 1)
        guard let url = link(at: at) ?? link(at: range.location) else { return nil }
        let title = InlineMarkdown.wikiTarget(url).map { "Open \u{201C}\($0)\u{201D}" } ?? "Open Link"
        let open = UIAction(title: title, image: UIImage(systemName: "arrow.up.right.square")) { [weak self] _ in self?.follow(url) }
        return UIMenu(children: [open] + suggestedActions)
    }

    // MARK: taps: checkboxes, ⌘-click on links

    @objc func tapped(_ g: UITapGestureRecognizer) {
        let tv = textView
        let p = g.location(in: tv)
        guard let pos = tv.closestPosition(to: p) else { return }
        let loc = tv.offset(from: tv.beginningOfDocument, to: pos)
        if editable, g.modifierFlags.contains(.command), let url = link(at: loc) ?? link(at: loc - 1) {
            follow(url)
            return
        }
        let s = tv.textStorage
        let ls = Surface.lines(s)
        guard !ls.isEmpty else { return }
        let li = Surface.lineIndex(ls, at: min(loc, s.length - 1))
        let l = ls[li]
        guard let prefix = l.style.prefix, let checked = prefix.checkbox else { return }
        // only a tap on the box itself (left of the text)
        let indent = CGFloat(prefix.indent) * SurfaceLook.indentStep
        guard p.x < tv.textContainerInset.left + indent + 24 else { return }
        guard let r = textRange(l.range), let tlm = tv.textLayoutManager,
              let frag = tlm.textLayoutFragment(for: r.location) else { return }
        let y = frag.layoutFragmentFrame
        guard p.y >= y.minY + tv.textContainerInset.top - 4, p.y <= y.maxY + tv.textContainerInset.top + 4 else { return }
        guard editable || onTodoCheckbox != nil else { return }
        setChecked(line: li, !checked)
    }

    func gestureRecognizer(_ g: UIGestureRecognizer, shouldRecognizeSimultaneouslyWith other: UIGestureRecognizer) -> Bool { true }
}

/// Text dropped from another app: inserted through `Surface.replace`, so
/// its lines become blocks the usual way (UIKit's own insert is cancelled).
extension SurfaceModel: UITextDropDelegate {
    func textDroppableView(_ textDroppableView: UIView & UITextDroppable, proposalForDrop drop: UITextDropRequest) -> UITextDropProposal {
        guard editable, drop.dropSession.canLoadObjects(ofClass: NSString.self) else { return UITextDropProposal(operation: .forbidden) }
        let proposal = UITextDropProposal(operation: .copy)
        // we insert it (willPerformDrop), the view doesn't
        proposal.dropPerformer = .delegate
        return proposal
    }

    func textDroppableView(_ textDroppableView: UIView & UITextDroppable, willPerformDrop drop: UITextDropRequest) {
        // where it goes, as (block, offset in it): the text may change
        // while the dropped items load
        let at = textView.offset(from: textView.beginningOfDocument, to: drop.dropPosition)
        let s = textView.textStorage
        let anchor = Surface.block(at: min(at, max(0, s.length - 1)), in: s).map { ($0.id, at - $0.range.location) }
        _ = drop.dropSession.loadObjects(ofClass: NSString.self) { [weak self] items in
            guard let text = items.first as? String else { return }
            Task { @MainActor in
                guard let self else { return }
                self.dropText(text, at: self.resolve(anchor, fallback: at))
            }
        }
    }

    func textDroppableView(_ textDroppableView: UIView & UITextDroppable, dropSessionDidEnter session: any UIDropSession) {}

    /// A (block, offset) anchor in the text as it is now.
    func resolve(_ anchor: (BlockID, Int)?, fallback: Int) -> Int {
        let s = textView.textStorage
        guard let (id, off) = anchor, let b = Surface.blocks(s).first(where: { $0.id == id }) else { return min(fallback, max(0, s.length - 1)) }
        return b.range.location + min(off, max(0, b.range.length - 1))
    }

    /// Insert dropped text at `location`.
    func dropText(_ text: String, at location: Int) {
        guard editable else { return }
        let s = textView.textStorage
        let at = min(max(0, location), max(0, s.length - 1))
        perform(Surface.replace(s, NSRange(location: at, length: 0), with: text, typing: SurfaceLook.typing(s, at: at, pending: nil, ctx: lookContext)))
    }
}
