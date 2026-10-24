import SwiftUI
import TaisceKit
import UniformTypeIdentifiers

/// Where a dragged doc lands on a row: just above it, inside it, just below it.
enum DocDropZone: Equatable, Sendable {
    case before, into, after

    /// The zone for a pointer `y` points down a row `height` tall: the
    /// top and bottom quarters place it beside the row, the middle inside.
    static func at(_ y: CGFloat, height: CGFloat) -> DocDropZone {
        guard height > 0 else { return .into }
        if y < height * 0.25 { return .before }
        if y > height * 0.75 { return .after }
        return .into
    }

    var side: DocPlacement.Side? {
        switch self {
        case .before: .before
        case .after: .after
        case .into: nil
        }
    }
}

/// The row a drag is over and where on it, when the doc may go there.
struct DocTreeHover: Equatable {
    var id: DocID
    var zone: DocDropZone
}

/// How the tree is drawn: sidebar rows (iPad 40 pt, the Mac 28 pt) or the
/// Library tab's card rows (iPhone, 48 pt, with a hairline between).
enum DocTreeStyle {
    case sidebar, library

    var rowHeight: CGFloat {
        switch self {
        case .library: 48
        case .sidebar:
            #if targetEnvironment(macCatalyst)
            28
            #else
            40
            #endif
        }
    }

    var indent: CGFloat { self == .sidebar ? 16 : 22 }
    /// the disclosure chevron's strip
    var chevronWidth: CGFloat { self == .sidebar ? 16 : 28 }
    /// where a row's content starts (before the indent)
    var lead: CGFloat { self == .sidebar ? 4 : 6 }
    /// selected rows: a solid accent bar (the Mac) or a soft wash
    var solidSelection: Bool {
        #if targetEnvironment(macCatalyst)
        self == .sidebar
        #else
        false
        #endif
    }
}

/// Scrolls the tree's scroll view while a drag rests near its top or
/// bottom edge (a plain ScrollView doesn't, unlike a List): rows report the
/// pointer, the scroller nudges the position.
@MainActor @Observable
final class TreeScroller {
    var position = ScrollPosition(idType: DocID.self)
    /// the scroll view on screen, in global coordinates
    @ObservationIgnored var viewport: CGRect = .zero
    @ObservationIgnored var offset: CGFloat = 0
    @ObservationIgnored var maxOffset: CGFloat = 0
    /// what the bars cover at the top and bottom (a tab bar, a toolbar)
    @ObservationIgnored var insets: (top: CGFloat, bottom: CGFloat) = (0, 0)
    @ObservationIgnored private var last: Date = .distantPast
    /// when the drag came into an edge band (nil: not in one), and the last update
    @ObservationIgnored private var entered: Date?
    @ObservationIgnored private var seen: Date?

    /// The edge band (points) that scrolls, how long a drag rests there
    /// before it does (a drop on the last visible row isn't scrolled away),
    /// and how far one nudge goes.
    static let band: CGFloat = 36
    static let dwell: TimeInterval = 0.4
    static let step: CGFloat = 18

    /// A drag is at global `y`: near an edge, scroll toward it (at most
    /// every 30 ms: dropUpdated comes as often as every frame).
    func drag(at y: CGFloat) {
        // the part you can see: not under the bars
        let top = viewport.minY + insets.top, bottom = viewport.maxY - insets.bottom
        guard bottom - top > Self.band * 3 else { return }
        let now = Date()
        // a gap in the updates: a new drag (the last one ended in the band)
        if let seen, now.timeIntervalSince(seen) > 0.3 { entered = nil }
        seen = now
        let inBand = y < top + Self.band || y > bottom - Self.band
        guard inBand else { entered = nil; return }
        if entered == nil { entered = now }
        guard now.timeIntervalSince(entered ?? now) >= Self.dwell, now.timeIntervalSince(last) > 0.03 else { return }
        var to = offset
        if y < top + Self.band { to = max(0, offset - Self.step) }
        else if y > bottom - Self.band { to = min(maxOffset, offset + Self.step) }
        guard to != offset else { return }
        last = Date()
        offset = to
        position.scrollTo(y: to)
    }
}

extension EnvironmentValues {
    @Entry var treeScroller: TreeScroller?
}

extension View {
    /// The scroll view a LibraryTree lives in: drags near its edges scroll it.
    func treeScrolling(_ scroller: TreeScroller) -> some View {
        self
            .scrollPosition(Bindable(scroller).position)
            .onScrollGeometryChange(for: [CGFloat].self) { g in
                [g.contentOffset.y + g.contentInsets.top, max(0, g.contentSize.height - g.containerSize.height + g.contentInsets.top + g.contentInsets.bottom),
                 g.contentInsets.top, g.contentInsets.bottom]
            } action: { _, v in
                scroller.offset = v[0]
                scroller.maxOffset = v[1]
                scroller.insets = (v[2], v[3])
            }
            .onGeometryChange(for: CGRect.self) { $0.frame(in: .global) } action: { scroller.viewport = $0 }
            .environment(\.treeScroller, scroller)
    }
}

/// One visible line of the tree.
struct DocTreeLine: Identifiable, Equatable {
    var node: LibraryNode
    var depth: Int
    var isOpen: Bool
    var id: DocID { node.id }

    /// The rows showing for `nodes` with these folders open, in order.
    static func lines(_ nodes: [LibraryNode], isOpen: (DocID) -> Bool, depth: Int = 0) -> [DocTreeLine] {
        var out: [DocTreeLine] = []
        for n in nodes {
            let open = n.isFolder && isOpen(n.id)
            out.append(DocTreeLine(node: n, depth: depth, isOpen: open))
            if open, let kids = n.children { out += lines(kids, isOpen: isOpen, depth: depth + 1) }
        }
        return out
    }
}

/// The Library tree, drawn row by row in a plain stack. Not a `List`:
/// List rows on iOS and the Mac never receive drops, so a doc could be
/// picked up but put nowhere. Here every row is a drop target with three
/// zones (above: before it, middle: inside it, below: after it), a
/// collapsed folder opens when a drag rests on it, and the space under the
/// last row is the top level. Click or tap opens a doc; the chevron opens
/// and closes a folder; arrow keys move the selection (left and right
/// close and open folders).
struct LibraryTree<Menu: View>: View {
    let nodes: [LibraryNode]
    @Binding var expanded: Set<DocID>
    /// folders that must stay open (the ones above the doc in front)
    var reveal: Set<DocID> = []
    var selected: DocID?
    var pins: Set<DocID> = []
    /// open review items per doc: a flag pill (Library) or a dot (sidebar)
    var flags: [DocID: Int] = [:]
    /// when a doc was last edited, if known (the Library's trailing time)
    var edited: (DocID) -> Date? = { _ in nil }
    var style: DocTreeStyle = .sidebar
    let onOpen: (DocID) -> Void
    /// a row came on screen
    var onRowAppear: (DocID) -> Void = { _ in }
    @ViewBuilder var menu: (LibraryNode) -> Menu

    @Environment(\.docMover) private var docMover
    /// the flags come from here when none are passed (the sidebar)
    @Environment(AppModel.self) private var model: AppModel?
    @State private var hover: DocTreeHover?
    @State private var bottomTargeted = false
    @FocusState private var focused: Bool

    private func isOpen(_ id: DocID) -> Bool { expanded.contains(id) || reveal.contains(id) }

    var lines: [DocTreeLine] { DocTreeLine.lines(nodes, isOpen: isOpen) }

    var body: some View {
        let lines = self.lines
        VStack(spacing: 0) {
            ForEach(Array(lines.enumerated()), id: \.element.id) { i, line in
                DocTreeRow(
                    line: line,
                    firstChild: line.isOpen ? line.node.children?.first?.id : nil,
                    style: style,
                    selected: selected == line.id,
                    pinned: pins.contains(line.id),
                    flags: (flags.isEmpty ? model?.reviewCounts.counts[line.id] : flags[line.id]) ?? 0,
                    edited: line.node.isFolder ? nil : edited(line.id),
                    separator: style == .library && i > 0,
                    hover: hover?.id == line.id ? hover?.zone : nil,
                    mover: docMover,
                    onOpen: {
                        // a click gives the sidebar the arrow keys
                        if style == .sidebar { focused = true }
                        onOpen(line.id)
                    },
                    onToggle: { toggle(line) },
                    setHover: { setHover(line.id, $0) },
                    // dropped inside: open it, so the doc stays in view
                    onDroppedInto: { withAnimation(.snappy(duration: 0.2)) { _ = expanded.insert(line.id) } }
                )
                .contextMenu { menu(line.node) }
                .id(line.id)
                .onAppear { if style == .library, !line.node.isFolder { onRowAppear(line.id) } }
                if line.isOpen, line.node.children == nil {
                    EmptyFolderLine(folder: line.id, depth: line.depth + 1, style: style, mover: docMover) {
                        withAnimation(.snappy(duration: 0.2)) { _ = expanded.insert(line.id) }
                    }
                }
            }
            // under the last row: the top level
            if let docMover {
                Color.clear
                    .frame(maxWidth: .infinity, minHeight: style == .library ? 10 : style.rowHeight * 2)
                    .contentShape(.rect)
                    .overlay(alignment: .top) {
                        if bottomTargeted {
                            Text("Move to the top level")
                                .font(.caption.weight(.medium))
                                .foregroundStyle(Color.lapis.accent)
                                .padding(.horizontal, 10)
                                .padding(.vertical, 4)
                                .background(Color.lapis.accentSoft, in: .capsule)
                                .background(Color.lapis.pal, in: .capsule)
                                .padding(.top, style == .library ? -10 : 6)
                                .allowsHitTesting(false)
                        }
                    }
                    .onDrop(of: [.taisceDoc], delegate: DocDropDelegate(target: nil, mover: docMover, targeted: $bottomTargeted))
                    .accessibilityHidden(true)
            }
        }
        // the flags: open review items per doc (throttled in ReviewCounts)
        .task(id: nodes.count) { await model?.reviewCounts.refresh(model?.api) }
        // a drag resting on a closed folder's middle opens it
        .task(id: hover) {
            // not an empty one: there is nothing inside to show, and opening
            // it (its "Empty" line) changed the tree under the drag and lost the drop
            guard let h = hover, h.zone == .into,
                  let line = lines.first(where: { $0.id == h.id }), line.node.children != nil, !line.isOpen else { return }
            try? await Task.sleep(for: .milliseconds(700))
            guard !Task.isCancelled, hover == h else { return }
            withAnimation(.snappy(duration: 0.2)) { _ = expanded.insert(h.id) }
        }
        .modifier(TreeKeys(enabled: style == .sidebar, focused: $focused,
                           step: { step(lines, $0) }, open: { openOrEnter(lines) }, close: { closeOrLeave(lines) }))
    }

    private func setHover(_ id: DocID, _ zone: DocDropZone?) {
        if let zone {
            let h = DocTreeHover(id: id, zone: zone)
            if hover != h { hover = h }
        } else if hover?.id == id {
            hover = nil
        }
    }

    private func toggle(_ line: DocTreeLine) {
        guard line.node.isFolder else { return }
        withAnimation(.snappy(duration: 0.2)) {
            if line.isOpen { expanded.remove(line.id) } else { expanded.insert(line.id) }
        }
    }

    // MARK: keyboard

    private func step(_ lines: [DocTreeLine], _ by: Int) -> KeyPress.Result {
        guard !lines.isEmpty else { return .ignored }
        let i = selected.flatMap { s in lines.firstIndex { $0.id == s } }
        let next = i.map { min(max($0 + by, 0), lines.count - 1) } ?? (by > 0 ? 0 : lines.count - 1)
        onOpen(lines[next].id)
        return .handled
    }

    private func openOrEnter(_ lines: [DocTreeLine]) -> KeyPress.Result {
        guard let s = selected, let i = lines.firstIndex(where: { $0.id == s }), lines[i].node.isFolder else { return .ignored }
        if lines[i].isOpen {
            if i + 1 < lines.count, lines[i + 1].depth > lines[i].depth { onOpen(lines[i + 1].id) }
        } else {
            toggle(lines[i])
        }
        return .handled
    }

    private func closeOrLeave(_ lines: [DocTreeLine]) -> KeyPress.Result {
        guard let s = selected, let i = lines.firstIndex(where: { $0.id == s }) else { return .ignored }
        if lines[i].isOpen, !reveal.contains(lines[i].id) {
            toggle(lines[i])
        } else if let parent = lines[..<i].last(where: { $0.depth == lines[i].depth - 1 }) {
            onOpen(parent.id)
        }
        return .handled
    }
}

/// Arrow keys in the sidebar (a hardware keyboard: the Mac, an iPad).
private struct TreeKeys: ViewModifier {
    let enabled: Bool
    var focused: FocusState<Bool>.Binding
    let step: (Int) -> KeyPress.Result
    let open: () -> KeyPress.Result
    let close: () -> KeyPress.Result

    func body(content: Content) -> some View {
        if enabled {
            content
                .focusable()
                .focused(focused)
                .focusEffectDisabled()
                .onKeyPress(.downArrow) { step(1) }
                .onKeyPress(.upArrow) { step(-1) }
                .onKeyPress(.rightArrow) { open() }
                .onKeyPress(.leftArrow) { close() }
        } else {
            content
        }
    }
}

/// A row: indent, chevron, icon, title, flags, and what's in it or when it
/// changed; the drag source and a drop target.
private struct DocTreeRow: View {
    let line: DocTreeLine
    /// an open folder's first child: a drop on its bottom edge goes there
    let firstChild: DocID?
    let style: DocTreeStyle
    let selected: Bool
    let pinned: Bool
    /// open review items on this doc
    var flags = 0
    /// when the doc was last edited (Library rows)
    var edited: Date?
    /// a hairline above (Library rows after the first)
    var separator = false
    /// the zone a drag is over on this row (nil: none here)
    let hover: DocDropZone?
    let mover: DocMover?
    let onOpen: () -> Void
    let onToggle: () -> Void
    let setHover: (DocDropZone?) -> Void
    let onDroppedInto: () -> Void

    @State private var pointer = false
    @State private var height: CGFloat = 0
    /// the row's top on screen, for edge scrolling: kept out of SwiftUI's
    /// sight, so scrolling doesn't redraw every row
    @State private var place = RowPlace()
    @Environment(\.treeScroller) private var scroller
    /// large text: the chevron's strip grows with the words (else it overlaps them)
    @ScaledMetric(relativeTo: .body) private var typeScale: CGFloat = 1
    private var chevronW: CGFloat { style.chevronWidth * max(1, typeScale) }

    private var node: LibraryNode { line.node }
    private var lead: CGFloat { CGFloat(line.depth) * style.indent }
    /// selected on the Mac: white on the accent
    private var onAccent: Bool { selected && style.solidSelection }

    var body: some View {
        HStack(spacing: style == .library ? 8 : 6) {
            chevron
            DocTreeIcon(isFolder: node.isFolder, isOpen: line.isOpen, size: style == .library ? .library : .sidebar, onAccent: onAccent)
            Text(node.doc.title)
                .font(style == .library ? .body : (style.solidSelection ? .body : .callout))
                .fontWeight(node.isFolder && style == .library ? .semibold : (selected && !style.solidSelection ? .semibold : nil))
                .foregroundStyle(onAccent ? Color.lapis.onAccent : Color.lapis.ink)
                .lineLimit(typeScale > 1.5 ? 3 : 1)
                .truncationMode(.tail)
                .layoutPriority(1)
            if flags > 0 {
                if style == .library {
                    FlagPill(count: flags)
                } else {
                    Circle().fill(Color.lapis.yellowMark).frame(width: 8, height: 8).accessibilityHidden(true)
                }
            }
            Spacer(minLength: 4)
            if pinned {
                Image(systemName: "pin.fill").font(.caption2).foregroundStyle(onAccent ? Color.lapis.onAccent : Color.lapis.accent).accessibilityHidden(true)
            }
            if let meta {
                Text(meta)
                    .font(style == .library ? .subheadline : .footnote)
                    .monospacedDigit()
                    .foregroundStyle(onAccent ? Color.lapis.onAccent.opacity(0.85) : Color.lapis.ink3)
                    .lineLimit(1)
                    .fixedSize()
                    .accessibilityHidden(true)
            }
        }
        .padding(.leading, lead + style.lead)
        .padding(.trailing, style == .library ? 14 : 10)
        .frame(maxWidth: .infinity, minHeight: style.rowHeight, alignment: .leading)
        .background(background)
        .overlay(alignment: .top) {
            if separator {
                Rectangle().fill(Color.lapis.line2).frame(height: 1)
                    .padding(.leading, lead + style.lead + chevronW + 8 + 22 * max(1, typeScale) + 8)
                    .allowsHitTesting(false)
            }
        }
        .overlay(alignment: .top) { if hover == .before { insertionLine } }
        .overlay(alignment: .bottom) { if hover == .after { insertionLine } }
        .contentShape(.rect)
        .onGeometryChange(for: CGRect.self) { $0.frame(in: .global) } action: { f in
            place.top = f.minY
            if height != f.height { height = f.height }
        }
        .onHover { pointer = $0 }
        // one gesture for the row: the chevron's strip opens and closes a
        // folder, anywhere else opens the doc (a Button inside a draggable
        // row doesn't reliably get the click on the Mac)
        .gesture(SpatialTapGesture().onEnded { tap in
            if node.isFolder, tap.location.x < lead + style.lead + chevronW + 6 { onToggle() } else { onOpen() }
        })
        .modifier(DragSource(id: node.id, title: node.doc.title, isFolder: node.isFolder, mover: mover))
        .modifier(RowDrop(id: node.id, firstChild: firstChild, height: height, mover: mover, setHover: setHover, onDroppedInto: onDroppedInto,
                          nearEdge: { [place] y in scroller?.drag(at: place.top + y) }))
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(node.doc.title)
        .accessibilityValue(accessibilityValue)
        .accessibilityHint(node.isFolder ? (node.children?.count).map { $0 == 1 ? "1 doc inside" : "\($0) docs inside" } ?? "Empty" : "")
        .accessibilityAddTraits(selected ? [.isButton, .isSelected] : .isButton)
        .accessibilityAction { onOpen() }
        .modifier(FolderAction(isFolder: node.isFolder, isOpen: line.isOpen, toggle: onToggle))
        .accessibilityIdentifier("tree.\(node.doc.title)")
    }

    /// A folder: how many docs are in it. A doc (Library): when it changed.
    private var meta: String? {
        if node.isFolder {
            let n = node.children?.count ?? 0
            return n > 0 ? "\(n)" : nil
        }
        guard style == .library, let edited else { return nil }
        return ShortWhen.string(edited)
    }

    private var accessibilityValue: String {
        var parts: [String] = []
        if node.isFolder { parts.append(line.isOpen ? "folder, open" : "folder, closed") }
        if flags > 0 { parts.append(flags == 1 ? "1 change to review" : "\(flags) changes to review") }
        if pinned { parts.append("pinned") }
        if !node.isFolder, let edited { parts.append(ShortWhen.spoken(edited)) }
        return parts.joined(separator: ", ")
    }

    @ViewBuilder private var chevron: some View {
        if node.isFolder {
            Image(systemName: "chevron.right")
                .font((style == .library ? Font.footnote : .caption).weight(.bold))
                .foregroundStyle(onAccent ? Color.lapis.onAccent : Color.lapis.ink3)
                .rotationEffect(.degrees(line.isOpen ? 90 : 0))
                .animation(Lapis.Motion.fast, value: line.isOpen)
                .frame(width: chevronW, height: style.rowHeight)
                .accessibilityHidden(true)
        } else {
            Color.clear.frame(width: chevronW, height: 1)
        }
    }

    private var background: some View {
        RoundedRectangle(cornerRadius: style == .library ? 0 : (style.solidSelection ? 7 : 12), style: .continuous)
            .fill(fill)
            .overlay {
                if hover == .into {
                    RoundedRectangle(cornerRadius: style == .library ? 12 : 7, style: .continuous).strokeBorder(Color.lapis.accent, lineWidth: 2)
                }
            }
            .padding(.leading, style == .library ? 0 : lead)
    }

    private var fill: Color {
        if hover == .into { return Color.lapis.accentRing }
        if selected { return style.solidSelection ? Color.lapis.accent : Color.lapis.accentSoft }
        if pointer { return Color.lapis.hover }
        return .clear
    }

    /// Where the doc will go beside this row: a line at its indent.
    private var insertionLine: some View {
        HStack(spacing: 0) {
            Circle().strokeBorder(Color.lapis.accent, lineWidth: 2).frame(width: 8, height: 8)
            Rectangle().fill(Color.lapis.accent).frame(height: 2)
        }
        .padding(.leading, lead + style.lead + style.chevronWidth)
        .offset(y: hover == .before ? -4 : 4)
        .allowsHitTesting(false)
    }
}

/// A tree row's icon: an accent folder with a soft fill, or a doc page.
struct DocTreeIcon: View {
    enum Size { case library, sidebar }
    let isFolder: Bool
    let isOpen: Bool
    var size: Size = .library
    var onAccent = false
    /// grows with the text (large Dynamic Type)
    @ScaledMetric(relativeTo: .body) private var width: CGFloat = 22

    var body: some View {
        Group {
            if isFolder {
                ZStack {
                    Image(systemName: "folder.fill").foregroundStyle(onAccent ? Color.lapis.onAccent.opacity(0.25) : Color.lapis.accentSoft)
                    Image(systemName: "folder").foregroundStyle(onAccent ? Color.lapis.onAccent : Color.lapis.accent)
                }
            } else {
                Image(systemName: "doc.text").foregroundStyle(onAccent ? Color.lapis.onAccent : Color.lapis.ink3)
            }
        }
        .font(size == .library ? .body : .callout)
        .frame(width: width)
        .accessibilityHidden(true)
    }
}

/// "⚑ 1": open review items on a doc (yellow, a flag and a number).
struct FlagPill: View {
    let count: Int

    var body: some View {
        if count > 0 {
            HStack(spacing: 3) {
                Image(systemName: "flag.fill").font(.system(size: 9, weight: .bold))
                Text("\(count)").font(.caption.weight(.semibold)).monospacedDigit()
            }
            .foregroundStyle(Color.lapis.yellow)
            .padding(.horizontal, 7)
            .frame(minHeight: 20)
            .background(Color.lapis.yellowSoft, in: .capsule)
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(count == 1 ? "1 change to review" : "\(count) changes to review")
        }
    }
}

/// Inside an open empty folder: says so, and a drop on it goes in.
private struct EmptyFolderLine: View {
    let folder: DocID
    let depth: Int
    let style: DocTreeStyle
    let mover: DocMover?
    let onDropped: () -> Void
    @State private var targeted = false

    var body: some View {
        Text(targeted ? "Drop it in here" : "Empty \u{2014} drag docs here")
            .font(.caption)
            .foregroundStyle(targeted ? Color.lapis.accent : Color.lapis.ink3)
            .padding(.leading, CGFloat(depth) * style.indent + style.lead + style.chevronWidth + 6)
            .frame(maxWidth: .infinity, minHeight: style.rowHeight * 0.8, alignment: .leading)
            .background(RoundedRectangle(cornerRadius: 7, style: .continuous)
                .fill(targeted ? Color.lapis.accentSoft : .clear)
                .padding(.leading, CGFloat(depth) * style.indent))
            .contentShape(.rect)
            .modifier(EmptyFolderDrop(folder: folder, mover: mover, targeted: $targeted, onDropped: onDropped))
            .accessibilityElement(children: .ignore)
            .accessibilityLabel("Empty folder")
            .accessibilityIdentifier("tree.empty")
    }
}

private struct EmptyFolderDrop: ViewModifier {
    let folder: DocID
    let mover: DocMover?
    @Binding var targeted: Bool
    let onDropped: () -> Void

    func body(content: Content) -> some View {
        if let mover {
            content.onDrop(of: [.taisceDoc], delegate: DocDropDelegate(target: folder, mover: mover, targeted: $targeted, onDropped: onDropped))
        } else {
            content
        }
    }
}

/// Not observed: written on every scroll frame.
private final class RowPlace {
    var top: CGFloat = 0
}

/// Expand / Collapse for VoiceOver, on folders only.
private struct FolderAction: ViewModifier {
    let isFolder: Bool
    let isOpen: Bool
    let toggle: () -> Void

    func body(content: Content) -> some View {
        if isFolder {
            content.accessibilityAction(named: isOpen ? "Collapse" : "Expand") { toggle() }
        } else {
            content
        }
    }
}

/// The row as a drag source: the doc id under our own type, visible to
/// this app only.
private struct DragSource: ViewModifier {
    let id: DocID
    let title: String
    let isFolder: Bool
    let mover: DocMover?

    func body(content: Content) -> some View {
        if let mover, mover.canMove(id) {
            content.onDrag {
                mover.began(id)
                return DocDrag.provider(id)
            } preview: {
                Label(title, systemImage: isFolder ? "folder" : "doc.text")
                    .font(.body)
                    .foregroundStyle(Color.lapis.ink)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 8)
                    .background(Color.lapis.card, in: .rect(cornerRadius: 8, style: .continuous))
            }
        } else {
            content
        }
    }
}

/// The row as a drop target with three zones.
private struct RowDrop: ViewModifier {
    let id: DocID
    let firstChild: DocID?
    let height: CGFloat
    let mover: DocMover?
    let setHover: (DocDropZone?) -> Void
    let onDroppedInto: () -> Void
    let nearEdge: (CGFloat) -> Void

    func body(content: Content) -> some View {
        if let mover {
            content.onDrop(of: [.taisceDoc], delegate: DocRowDropDelegate(target: id, firstChild: firstChild, height: height, mover: mover, setHover: setHover,
                                                                           onDroppedInto: onDroppedInto, nearEdge: nearEdge))
        } else {
            content
        }
    }
}

/// Drops on a tree row: the zone under the pointer decides before, inside
/// or after; a zone the dragged doc can't go to shows as forbidden and
/// draws nothing.
struct DocRowDropDelegate: DropDelegate {
    let target: DocID
    /// set for an open folder: just below its row is its first child's spot
    var firstChild: DocID? = nil
    let height: CGFloat
    let mover: DocMover
    let setHover: (DocDropZone?) -> Void
    var onDroppedInto: () -> Void = {}
    /// the pointer's y in the row: the scroll view scrolls near its edges
    var nearEdge: (CGFloat) -> Void = { _ in }

    /// Where a zone puts the doc: beside a row, or for an open folder's
    /// bottom edge, just before its first child (the line draws there).
    func spot(_ zone: DocDropZone) -> (side: DocPlacement.Side, target: DocID)? {
        guard let side = zone.side else { return nil }
        if side == .after, let firstChild { return (.before, firstChild) }
        return (side, target)
    }

    func allowed(_ zone: DocDropZone) -> Bool {
        guard let dragged = mover.dragging() else { return false }
        if dragged == target { return false }
        if let s = spot(zone) { return s.target != dragged && mover.canPlace(dragged, s.side, s.target) }
        return mover.canDrop(dragged, target)
    }

    func validateDrop(info: DropInfo) -> Bool {
        return info.hasItemsConforming(to: [.taisceDoc])
    }

    func dropEntered(info: DropInfo) {
        _ = dropUpdated(info: info)
    }

    func dropUpdated(info: DropInfo) -> DropProposal? {
        nearEdge(info.location.y)
        let zone = DocDropZone.at(info.location.y, height: height)
        if allowed(zone) {
            setHover(zone)
            return DropProposal(operation: .move)
        }
        setHover(nil)
        return DropProposal(operation: .forbidden)
    }

    func dropExited(info: DropInfo) {
        setHover(nil)
    }

    func performDrop(info: DropInfo) -> Bool {
        let zone = DocDropZone.at(info.location.y, height: height)
        setHover(nil)
        guard info.hasItemsConforming(to: [.taisceDoc]) else { return false }
        // the doc the drag carries, not the one noted when a drag began
        let (target, s, mover, onDroppedInto) = (self.target, spot(zone), self.mover, self.onDroppedInto)
        DocDrag.dropped(info, hint: mover.dragging()) { dragged in
            guard dragged != target else { return }
            if let s {
                if s.target != dragged { _ = mover.place(dragged, s.side, s.target) }
                return
            }
            if mover.drop(dragged, target) { onDroppedInto() }
        }
        return true
    }
}
