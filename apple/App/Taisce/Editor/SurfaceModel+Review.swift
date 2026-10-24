import SwiftUI
import UIKit
import TaisceKit

// MARK: - inline change cards

extension SurfaceModel {
    /// how far a card reaches past the text column on each side
    static let cardBleed: CGFloat = 10
    /// the card's top above the block's first line
    static let cardLead: CGFloat = 10

    /// Place a card over each block with an open review item, and leave
    /// room under the block's text for it (paragraph spacing, restyled).
    /// Called from the text view's layout when the text, the items or the
    /// width changed.
    func layoutCards() {
        let tv = textView
        let items = cardItems
        for (id, v) in tv.cardViews where items[id] == nil {
            v.removeFromSuperview()
            tv.cardViews[id] = nil
            cardHosts[id] = nil
        }
        var stale = cardSpace.keys.filter { items[$0] == nil }
        for id in stale { cardSpace[id] = nil }
        guard !items.isEmpty, let tlm = tv.textLayoutManager, let cm = tlm.textContentManager else {
            if !stale.isEmpty { restyleSoon() }
            return
        }
        let width = tv.columnWidth + Self.cardBleed * 2
        guard width > 120 else { return }
        let docStart = cm.documentRange.location
        for b in Surface.blocks(tv.textStorage) {
            guard let item = items[b.id], let range = textRange(b.range) else { continue }
            let host = cardHost(b.id, item: item)
            let size = host.sizeThatFits(in: CGSize(width: width, height: .greatestFiniteMagnitude))
            let cardHeight = ceil(size.height)
            // the block's text, from its line fragments (spacing excluded)
            var top = CGFloat.greatestFiniteMagnitude, bottom = -CGFloat.greatestFiniteMagnitude
            let end = b.range.location + b.range.length
            // from the top: fragments above may still sit at estimated heights
            // (a card above just grew its block's spacing), which moves this one
            if let upTo = NSTextRange(location: docStart, end: range.endLocation) { tlm.ensureLayout(for: upTo) }
            tlm.enumerateTextLayoutFragments(from: range.location, options: [.ensuresLayout]) { frag in
                let at = cm.offset(from: docStart, to: frag.rangeInElement.location)
                guard at < end else { return false }
                let o = frag.layoutFragmentFrame.origin
                for line in frag.textLineFragments {
                    let r = line.typographicBounds.offsetBy(dx: o.x, dy: o.y)
                    top = min(top, r.minY)
                    bottom = max(bottom, r.maxY)
                }
                return true
            }
            guard top < bottom else { continue }
            let need = max(0, cardHeight - (bottom - top) - Self.cardLead) + 12
            if abs((cardSpace[b.id] ?? -1) - need) > 1 {
                cardSpace[b.id] = need
                stale.append(b.id)
            }
            let view = host.view!
            if view.superview !== tv {
                tv.addSubview(view)
                tv.cardViews[b.id] = view
            }
            view.frame = CGRect(x: tv.textContainerInset.left - Self.cardBleed, y: tv.textContainerInset.top + top - Self.cardLead,
                                width: width, height: cardHeight)
        }
        if !stale.isEmpty { restyleSoon() }
    }

    /// Restyle after this layout pass (never inside one).
    private func restyleSoon() {
        DispatchQueue.main.async { [weak self] in
            guard let self else { return }
            self.restyle()
            self.textView.cardsDirty = true
            self.textView.setNeedsLayout()
        }
    }

    private func cardHost(_ id: BlockID, item: DocReviewItem) -> UIHostingController<AnyView> {
        let view = AnyView(SurfaceChangeCard(model: self, item: item)
            .environment(\.docScale, docScale)
            .dynamicTypeSize(dynamicType))
        if let h = cardHosts[id] {
            h.rootView = view
            return h
        }
        let h = UIHostingController(rootView: view)
        h.view.backgroundColor = .clear
        h.sizingOptions = []
        cardHosts[id] = h
        return h
    }

    /// Bring a block into view (an outline heading, a review item).
    func scrollTo(block id: BlockID) {
        let tv = textView
        guard let b = Surface.blocks(tv.textStorage).first(where: { $0.id == id }), let range = textRange(b.range),
              let tlm = tv.textLayoutManager else { return }
        tlm.ensureLayout(for: range)
        guard let frag = tlm.textLayoutFragment(for: range.location) else { return }
        let y = frag.layoutFragmentFrame.minY + tv.textContainerInset.top - 24
        let maxY = max(-tv.adjustedContentInset.top, tv.contentSize.height - tv.bounds.height + tv.adjustedContentInset.bottom)
        let to = min(max(-tv.adjustedContentInset.top, y - tv.adjustedContentInset.top), maxY)
        tv.setContentOffset(CGPoint(x: 0, y: to), animated: !UIAccessibility.isReduceMotionEnabled)
        UIAccessibility.post(notification: .layoutChanged, argument: tv.cardViews[id] ?? tv)
    }

    /// Find in Doc (⌘F, the … menu).
    func presentFind() {
        finder.present()
    }
}

/// The SwiftUI over a covered block: the review item as an inline change card.
struct SurfaceChangeCard: View {
    /// weak: the card's view lives in the text view the model owns
    weak var model: SurfaceModel?
    let item: DocReviewItem
    @Environment(\.horizontalSizeClass) private var sizeClass

    var body: some View {
        let card = ChangeCardModel(item)
        let model = model
        InlineChangeCard(
            verdict: card.verdict, author: card.author, when: card.date.map { ShortWhen.string($0) },
            old: card.old, new: card.new, note: card.note,
            onAccept: { Task { await model?.decide(item, .accept) } },
            onReject: { Task { await model?.decide(item, .decline) } },
            acceptTitle: card.acceptTitle, rejectTitle: card.rejectTitle,
            confirmAccept: item.kind == .parked && item.opType == "delete",
            showsActions: model?.editable ?? false, busy: model?.deciding.contains(item.id) ?? false,
            compact: sizeClass == .regular
        )
        // opaque: the block's own text sits under it
        .background(Color.lapis.bg, in: .rect(cornerRadius: 20, style: .continuous))
        .accessibilityIdentifier("change.card")
    }
}

// MARK: - find

/// Find in Doc: the system find bar over the surface, find only (a replace
/// UIKit applied itself would skip the surface's block ids).
@MainActor
final class SurfaceFinder: NSObject, UIFindInteractionDelegate {
    weak var model: SurfaceModel?
    lazy var interaction = UIFindInteraction(sessionDelegate: self)

    init(model: SurfaceModel) {
        self.model = model
    }

    func findInteraction(_ interaction: UIFindInteraction, sessionFor view: UIView) -> UIFindSession? {
        guard let tv = model?.textView else { return nil }
        let searchable: any UITextSearching = tv
        return FindOnlySession(searchableObject: searchable)
    }

    func present() {
        interaction.presentFindNavigator(showingReplace: false)
    }
}

private final class FindOnlySession: UITextSearchingFindSession {
    override var supportsReplacement: Bool { false }
}

// MARK: - checkboxes

/// TextKit 2 layout for the surface: a list checkbox line draws its box
/// itself, so ticked and unticked are one shape (the marker glyphs fall
/// back to an emoji for ☑, a thin outline for ☐).
final class SurfaceLayoutDelegate: NSObject, NSTextLayoutManagerDelegate {
    func textLayoutManager(_ textLayoutManager: NSTextLayoutManager, textLayoutFragmentFor location: any NSTextLocation,
                           in textElement: NSTextElement) -> NSTextLayoutFragment {
        if let p = textElement as? NSTextParagraph, let checked = Self.checkbox(p.attributedString) {
            let f = CheckboxLayoutFragment(textElement: textElement, range: textElement.elementRange)
            f.checked = checked
            return f
        }
        if let p = textElement as? NSTextParagraph, Self.hasCode(p.attributedString) {
            return SurfaceLayoutFragment(textElement: textElement, range: textElement.elementRange)
        }
        return NSTextLayoutFragment(textElement: textElement, range: textElement.elementRange)
    }

    /// Whether the line holds an inline code run.
    static func hasCode(_ s: NSAttributedString) -> Bool {
        var found = false
        s.enumerateAttribute(Surface.marks, in: NSRange(location: 0, length: s.length)) { v, _, stop in
            if InlineMarks(rawValue: (v as? Int) ?? 0).contains(.code) { found = true; stop.pointee = true }
        }
        return found
    }

    /// The line's box state, when it starts with a checkbox marker.
    static func checkbox(_ s: NSAttributedString) -> Bool? {
        guard s.length > 0, s.attribute(Surface.marker, at: 0, effectiveRange: nil) != nil,
              let enc = s.attribute(Surface.line, at: 0, effectiveRange: nil) as? String else { return nil }
        return Surface.LineStyle(encoded: enc).prefix?.checkbox
    }
}

/// A surface line that draws its inline code runs as rounded, outlined
/// pills under the text (the artboards' `code`: code fill, a hairline, 6 pt corners).
class SurfaceLayoutFragment: NSTextLayoutFragment {
    override var renderingSurfaceBounds: CGRect {
        super.renderingSurfaceBounds.insetBy(dx: -Self.outset - 1, dy: -2)
    }

    static let outset: CGFloat = 3

    override func draw(at point: CGPoint, in context: CGContext) {
        drawCodePills(at: point, in: context)
        super.draw(at: point, in: context)
    }

    func drawCodePills(at point: CGPoint, in context: CGContext) {
        for line in textLineFragments where line.characterRange.length > 0 {
            let attr = line.attributedString
            let lb = line.typographicBounds
            let baseline = point.y + lb.minY + line.glyphOrigin.y
            attr.enumerateAttribute(Surface.marks, in: line.characterRange) { v, r, _ in
                guard InlineMarks(rawValue: (v as? Int) ?? 0).contains(.code), r.length > 0 else { return }
                // a trailing newline is not part of the pill
                var run = r
                while run.length > 0, let c = (attr.string as NSString).substring(with: NSRange(location: run.location + run.length - 1, length: 1)).first,
                      c.isNewline {
                    run.length -= 1
                }
                guard run.length > 0 else { return }
                let font = (attr.attribute(.font, at: run.location, effectiveRange: nil) as? UIFont) ?? UIFont.monospacedSystemFont(ofSize: 15, weight: .regular)
                let x0 = line.locationForCharacter(at: run.location).x
                let x1 = line.locationForCharacter(at: run.location + run.length).x
                guard x1 > x0 else { return }
                let rect = CGRect(x: point.x + lb.minX + x0 - Self.outset, y: baseline - font.ascender - 2,
                                  width: x1 - x0 + Self.outset * 2, height: font.ascender - font.descender + 4)
                let path = UIBezierPath(roundedRect: rect.insetBy(dx: 0.5, dy: 0.5), cornerRadius: 6)
                context.saveGState()
                context.setFillColor(UIColor.lapis.code.cgColor)
                context.addPath(path.cgPath)
                context.fillPath()
                context.setStrokeColor(UIColor.lapis.line.cgColor)
                context.setLineWidth(1)
                context.addPath(path.cgPath)
                context.strokePath()
                context.restoreGState()
            }
        }
    }
}

final class CheckboxLayoutFragment: SurfaceLayoutFragment {
    var checked = false

    /// The box for a font: a rounded square a little under the cap height
    /// band, centred on the x-height.
    static func box(font: UIFont) -> CGSize {
        let s = round(font.pointSize * 0.86)
        return CGSize(width: s, height: s)
    }

    override func draw(at point: CGPoint, in context: CGContext) {
        guard let line = textLineFragments.first, line.characterRange.length > 0 else {
            super.draw(at: point, in: context)
            return
        }
        let attr = line.attributedString
        let start = line.characterRange.location
        let font = (attr.attribute(.font, at: start, effectiveRange: nil) as? UIFont) ?? UIFont.preferredFont(forTextStyle: .body)
        let lb = line.typographicBounds
        let x0 = line.locationForCharacter(at: start).x
        let x1 = line.characterRange.length > 1 ? line.locationForCharacter(at: start + 1).x : x0 + font.pointSize
        // the marker's own glyph never draws (clipped out); the box does
        let markerRect = CGRect(x: point.x + lb.minX + x0, y: point.y + lb.minY, width: max(1, x1 - x0), height: lb.height)
        context.saveGState()
        let all = CGRect(x: -10_000, y: -10_000, width: 20_000, height: 20_000)
        context.addRect(all)
        context.addRect(markerRect)
        context.clip(using: .evenOdd)
        super.draw(at: point, in: context)
        context.restoreGState()

        let size = Self.box(font: font)
        let baseline = point.y + lb.minY + line.glyphOrigin.y
        let midY = baseline - font.xHeight / 2
        let rect = CGRect(x: markerRect.minX, y: midY - size.height / 2, width: size.width, height: size.height).integral
        let radius = size.width * 0.28
        context.saveGState()
        if checked {
            let path = UIBezierPath(roundedRect: rect, cornerRadius: radius)
            context.setFillColor(UIColor.lapis[.greenFill].cgColor)
            context.addPath(path.cgPath)
            context.fillPath()
            let tick = UIBezierPath()
            tick.move(to: CGPoint(x: rect.minX + rect.width * 0.26, y: rect.minY + rect.height * 0.53))
            tick.addLine(to: CGPoint(x: rect.minX + rect.width * 0.43, y: rect.minY + rect.height * 0.70))
            tick.addLine(to: CGPoint(x: rect.minX + rect.width * 0.75, y: rect.minY + rect.height * 0.33))
            context.setStrokeColor(UIColor.white.cgColor)
            context.setLineWidth(max(1.6, size.width * 0.12))
            context.setLineCap(.round)
            context.setLineJoin(.round)
            context.addPath(tick.cgPath)
            context.strokePath()
        } else {
            let lw: CGFloat = max(1.4, size.width * 0.09)
            let path = UIBezierPath(roundedRect: rect.insetBy(dx: lw / 2, dy: lw / 2), cornerRadius: radius - lw / 2)
            context.setStrokeColor(UIColor.lapis.ink3.cgColor)
            context.setLineWidth(lw)
            context.addPath(path.cgPath)
            context.strokePath()
        }
        context.restoreGState()
    }
}
