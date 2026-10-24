import SwiftUI
import UIKit
import TaisceKit

/// A non-text block inside the surface: one U+FFFC drawn as the reading
/// view's own rendering (code with Run, diagrams, charts, tables, callouts…).
/// It is one character, so it selects and deletes as a unit; a tap on it
/// opens its markdown source (`SurfaceModel.editSource`).
final class BlockAttachment: NSTextAttachment {
    let blockID: BlockID
    let source: String
    let nodes: [RenderNode]
    weak var host: SurfaceModel?
    /// measured height per width (the hosted view can grow: a diagram that finished rendering)
    var heights: [Int: CGFloat] = [:]

    init(blockID: BlockID, source: String, nodes: [RenderNode], host: SurfaceModel?) {
        self.blockID = blockID
        self.source = source
        self.nodes = nodes
        self.host = host
        super.init(data: nil, ofType: nil)
        allowsTextAttachmentView = true
    }

    required init?(coder: NSCoder) { fatalError("not archived") }

    override func viewProvider(for parentView: UIView?, location: any NSTextLocation, textContainer: NSTextContainer?) -> NSTextAttachmentViewProvider? {
        let p = BlockAttachmentViewProvider(textAttachment: self, parentView: parentView, textLayoutManager: textContainer?.textLayoutManager, location: location)
        p.tracksTextAttachmentViewBounds = true
        return p
    }

    /// Plain text for copy and accessibility: the block's markdown.
    override var description: String { source }
}

final class BlockAttachmentViewProvider: NSTextAttachmentViewProvider {
    nonisolated(unsafe) private var hosting: UIHostingController<AnyView>?

    private var attachment: BlockAttachment? { textAttachment as? BlockAttachment }

    // TextKit calls these on the main thread
    override func loadView() {
        nonisolated(unsafe) let me = self
        MainActor.assumeIsolated { me.build() }
    }

    @MainActor private func build() {
        guard let a = attachment else { view = UIView(); return }
        let h = UIHostingController(rootView: a.host?.attachmentContent(a) ?? AnyView(EmptyView()))
        h.view.backgroundColor = .clear
        h.sizingOptions = []
        hosting = h
        view = h.view
    }

    @MainActor private func fit(_ width: CGFloat) -> CGSize {
        if hosting == nil { build() }
        return hosting?.sizeThatFits(in: CGSize(width: width, height: .greatestFiniteMagnitude)) ?? CGSize(width: width, height: 60)
    }

    override func attachmentBounds(
        for attributes: [NSAttributedString.Key: Any], location: any NSTextLocation, textContainer: NSTextContainer?,
        proposedLineFragment: CGRect, position: CGPoint
    ) -> CGRect {
        guard let a = attachment else { return .zero }
        let inset = textContainer?.lineFragmentPadding ?? 0
        let width = max(80, proposedLineFragment.width - inset * 2)
        let key = Int(width)
        if let h = a.heights[key] { return CGRect(x: 0, y: 0, width: width, height: h) }
        nonisolated(unsafe) let me = self
        let size = MainActor.assumeIsolated { me.fit(width) }
        let h = max(24, ceil(size.height))
        a.heights[key] = h
        return CGRect(x: 0, y: 0, width: width, height: h)
    }
}

/// The SwiftUI inside an attachment: the rendered block with its own
/// controls (Run, the try line, a practice edit, ticking a box). A plain tap
/// does nothing else; its source opens from the pencil, a double-tap, or
/// Return with the block selected. Reports size changes so the text lays
/// out around the new height.
struct AttachmentContent: View {
    let attachment: BlockAttachment
    let model: SurfaceModel
    let app: AppModel?

    var body: some View {
        let content = NodeStack(nodes: attachment.nodes)
            .environment(\.codeRunContext, model.docID.isEmpty ? nil : CodeRunContext(doc: model.docID, block: attachment.blockID, canSave: model.editable))
            .environment(\.docTables, model.chartTables)
            .environment(\.docScale, model.docScale)
            .environment(\.openURL, OpenURLAction { url in model.follow(url) ? .handled : .systemAction })
            // a box inside a callout, quote or complex list: ticked in the block's markdown
            .environment(\.checkboxToggle, CheckboxToggle(action: model.editable ? { i, v in model.toggleRawCheckbox(attachment.blockID, index: i, checked: v) } : nil))
            .frame(maxWidth: .infinity, alignment: .leading)
            .fixedSize(horizontal: false, vertical: true)
            .onGeometryChange(for: CGFloat.self) { ceil($0.size.height) } action: { h in
                model.attachmentResized(attachment, height: h)
            }
            .overlay(alignment: .topTrailing) {
                if model.editable {
                    Button {
                        model.editSource(attachment.blockID)
                    } label: {
                        Image(systemName: "pencil")
                            .font(.caption.weight(.semibold))
                            .padding(6)
                            .background(.thinMaterial, in: .circle)
                    }
                    .buttonStyle(.plain)
                    .padding(6)
                    .accessibilityLabel("Edit source")
                    .accessibilityIdentifier("surface.editSource")
                }
            }
            .contentShape(.rect)
            .onTapGesture(count: 2) { if model.editable { model.editSource(attachment.blockID) } }
            .accessibilityElement(children: .contain)
            .accessibilityLabel(Self.label(attachment.source))
            .accessibilityAction(named: "Edit source") { if model.editable { model.editSource(attachment.blockID) } }
            .accessibilityIdentifier("surface.block")
        if let app {
            content.environment(app)
        } else {
            content
        }
    }

    /// What VoiceOver calls a non-text block: "Code block, bash",
    /// "Diagram, mermaid", "Chart", "Table, 3 rows", "Callout, note"…
    nonisolated static func label(_ source: String) -> String {
        let first = source.split(separator: "\n", maxSplits: 1, omittingEmptySubsequences: false).first.map(String.init) ?? ""
        if first.hasPrefix("```") || first.hasPrefix("~~~") {
            let lang = first.dropFirst(3).trimmingCharacters(in: .whitespaces).split(separator: " ").first.map(String.init) ?? ""
            switch lang {
            case "mermaid", "d2", "reladraw": return "Diagram, \(lang)"
            case "vega-lite", "vega": return "Chart"
            case "": return "Code block"
            default: return "Code block, \(lang)"
            }
        }
        if first.hasPrefix("|") {
            let rows = source.split(separator: "\n").filter { $0.hasPrefix("|") }.count
            return "Table, \(max(0, rows - 2)) rows"
        }
        if first.hasPrefix("> [!") {
            let kind = first.dropFirst(4).prefix { $0 != "]" }.lowercased()
            return "Callout, \(kind)"
        }
        if first.hasPrefix(">") { return "Quote" }
        if first.hasPrefix("---") { return "Properties" }
        if first.hasPrefix("- ") || first.hasPrefix("* ") || first.first?.isNumber == true { return "List" }
        return "Block"
    }
}

/// A code editor for a block's markdown source: a UITextView with no smart
/// quotes or dashes, no autocorrect, spell check or capitalisation, in a
/// monospaced font (what main's editor did for raw blocks).
struct CodeTextEditor: UIViewRepresentable {
    @Binding var text: String

    static func configure(_ tv: UITextView) {
        tv.smartQuotesType = .no
        tv.smartDashesType = .no
        tv.smartInsertDeleteType = .no
        tv.autocorrectionType = .no
        tv.spellCheckingType = .no
        tv.autocapitalizationType = .none
        tv.keyboardType = .asciiCapable
        tv.font = UIFont.monospacedSystemFont(ofSize: UIFont.preferredFont(forTextStyle: .footnote).pointSize, weight: .regular)
        tv.adjustsFontForContentSizeCategory = true
        tv.backgroundColor = .clear
        tv.accessibilityIdentifier = "source.text"
    }

    func makeUIView(context: Context) -> UITextView {
        let tv = UITextView()
        Self.configure(tv)
        tv.delegate = context.coordinator
        tv.text = text
        return tv
    }

    func updateUIView(_ tv: UITextView, context: Context) {
        if tv.text != text { tv.text = text }
    }

    func makeCoordinator() -> Coordinator { Coordinator(text: $text) }

    final class Coordinator: NSObject, UITextViewDelegate {
        var text: Binding<String>
        init(text: Binding<String>) { self.text = text }
        func textViewDidChange(_ tv: UITextView) { text.wrappedValue = tv.text }
    }
}

/// The markdown source of one non-text block, edited in a sheet. Closing it
/// with changes asks first (inline); a block removed meanwhile (a refresh)
/// is said so, with the choice to put the text back as a new block.
struct SourceEditorSheet: View {
    let blockID: BlockID
    let original: String
    @State var text: String
    /// false: the block is gone
    let onSave: (String) -> Bool
    let onInsertAsNew: (String) -> Void
    let onDelete: () -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var confirmingDiscard = false
    @State private var blockGone = false

    init(blockID: BlockID, text: String, onSave: @escaping (String) -> Bool, onInsertAsNew: @escaping (String) -> Void, onDelete: @escaping () -> Void) {
        self.blockID = blockID
        original = text
        _text = State(initialValue: text)
        self.onSave = onSave
        self.onInsertAsNew = onInsertAsNew
        self.onDelete = onDelete
    }

    var edited: Bool { text != original }

    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                if blockGone {
                    note("This block was removed in the meantime (on another device).") {
                        Button("Insert as a new block") { onInsertAsNew(text); dismiss() }.accessibilityIdentifier("source.insertNew")
                        Button("Discard", role: .destructive) { dismiss() }
                    }
                } else if confirmingDiscard {
                    note("Discard your changes to this block?") {
                        Button("Discard", role: .destructive) { dismiss() }.accessibilityIdentifier("source.discard")
                        Button("Keep editing") { confirmingDiscard = false }
                    }
                }
                CodeTextEditor(text: $text)
                    .padding(.horizontal, 12)
                    .background(Color.lapis.card2)
            }
            .navigationTitle("Block source")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { if edited { confirmingDiscard = true } else { dismiss() } }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save") {
                        if onSave(text) { dismiss() } else { blockGone = true }
                    }
                    .accessibilityIdentifier("source.save")
                }
                ToolbarItem(placement: .bottomBar) {
                    Button("Delete block", role: .destructive) {
                        onDelete()
                        dismiss()
                    }
                }
            }
        }
        // a swipe down with changes doesn't drop them: Cancel asks
        .interactiveDismissDisabled(edited)
        .tint(Color.lapis.accent)
    }

    private func note(_ text: String, @ViewBuilder actions: () -> some View) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(text).font(.footnote)
            HStack(spacing: 16) { actions() }.font(.footnote.weight(.medium)).buttonStyle(.borderless)
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.lapis.yellow.opacity(0.12))
    }
}
