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

/// The SwiftUI inside an attachment: the rendered block, tappable to edit
/// its source when the surface is editable. Reports size changes so the
/// text lays out around the new height.
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
            .onTapGesture { if model.editable { model.editSource(attachment.blockID) } }
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier("surface.block")
        if let app {
            content.environment(app)
        } else {
            content
        }
    }
}

/// The markdown source of one non-text block, edited in a sheet.
struct SourceEditorSheet: View {
    let blockID: BlockID
    @State var text: String
    let onSave: (String) -> Void
    let onDelete: () -> Void
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            TextEditor(text: $text)
                .font(.system(.footnote, design: .monospaced))
                .autocorrectionDisabled()
                .textInputAutocapitalization(.never)
                .scrollContentBackground(.hidden)
                .padding(.horizontal, 12)
                .background(Theme.surface2)
                .accessibilityIdentifier("source.text")
                .navigationTitle("Block source")
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("Cancel") { dismiss() }
                    }
                    ToolbarItem(placement: .confirmationAction) {
                        Button("Save") {
                            onSave(text)
                            dismiss()
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
        .tint(Theme.accent)
    }
}
