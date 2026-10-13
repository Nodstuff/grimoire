import SwiftUI
import TaisceKit

/// The doc as one live surface: the text view (header and children ride
/// inside it), the formatting bar on the Mac, conflict and refusal notes,
/// the `[[` popup, and the source sheet for non-text blocks.
struct DocSurfaceContent: View {
    let model: SurfaceModel
    @Environment(\.dynamicTypeSize) private var dynamicType

    var body: some View {
        DocSurfaceView(model: model, dynamicType: dynamicType)
            .groundBackground()
            .safeAreaInset(edge: .top, spacing: 0) {
                VStack(spacing: 0) {
                    if model.editable, EditorChrome.barIsInline { InlineFormattingBar(bar: model.formattingBar) }
                    SurfaceNotes(model: model)
                }
            }
            .overlay { CompletionOverlay(model: model) }
            // a brief, non-blocking notice ("Images can't be pasted here yet")
            .overlay(alignment: .bottom) {
                if let notice = model.notice {
                    Text(notice)
                        .font(.footnote.weight(.medium))
                        .padding(.horizontal, 14)
                        .padding(.vertical, 8)
                        .glassEffect(.regular, in: .capsule)
                        .padding(.bottom, 16)
                        .accessibilityIdentifier("surface.notice")
                        .task(id: notice) {
                            try? await Task.sleep(for: .seconds(3))
                            if model.notice == notice { model.notice = nil }
                        }
                }
            }
            .sheet(item: Binding(get: { model.sourceEditing.map(SourceRef.init) }, set: { model.sourceEditing = $0?.id })) { ref in
                SourceEditorSheet(
                    blockID: ref.id, text: model.source(of: ref.id),
                    onSave: { model.saveSource(ref.id, $0) },
                    onInsertAsNew: { model.insertBlocks($0) },
                    onDelete: { model.deleteBlock(ref.id) }
                )
            }
    }
}

private struct SourceRef: Identifiable {
    let id: BlockID
}

/// The editor's UIToolbar, hosted in SwiftUI (the Mac).
private struct InlineFormattingBar: UIViewRepresentable {
    let bar: FormattingBar

    func makeUIView(context: Context) -> FormattingBar { bar }
    func updateUIView(_ view: FormattingBar, context: Context) {}
    func sizeThatFits(_ proposal: ProposedViewSize, uiView: FormattingBar, context: Context) -> CGSize? {
        CGSize(width: proposal.width ?? 320, height: 44)
    }
}

/// Blocks the server changed under an edit (Keep mine / Take theirs) and
/// saves it refused (Retry / Discard), above the text. The blocks
/// themselves are tinted in the text.
private struct SurfaceNotes: View {
    let model: SurfaceModel

    var body: some View {
        let conflicts = model.conflicts.filter { $0.value.theirs != nil }.sorted { $0.key < $1.key }
        if !conflicts.isEmpty || !model.failedBlocks.isEmpty {
            VStack(alignment: .leading, spacing: 8) {
                if !model.failedBlocks.isEmpty {
                    HStack(spacing: 12) {
                        Label(model.failedBlocks.count == 1 ? "1 block couldn't be saved" : "\(model.failedBlocks.count) blocks couldn't be saved",
                              systemImage: "exclamationmark.triangle")
                            .foregroundStyle(Theme.rose)
                        Spacer()
                        Button("Retry") { Task { await model.retryFailed() } }.accessibilityIdentifier("block.retry")
                        Button("Discard", role: .destructive) { Task { await model.discardFailed() } }.accessibilityIdentifier("block.discard")
                    }
                }
                ForEach(conflicts, id: \.key) { id, c in
                    VStack(alignment: .leading, spacing: 4) {
                        Label("Changed on another device while you edited", systemImage: "arrow.triangle.branch").foregroundStyle(Theme.rose)
                        Text(c.theirs ?? "").foregroundStyle(Theme.secondary).lineLimit(2)
                            .accessibilityLabel("Their version: \(c.theirs ?? "")")
                        HStack(spacing: 16) {
                            Button("Keep mine") { Task { await model.keepMine(id) } }.accessibilityIdentifier("block.keepMine")
                            Button("Take theirs") { Task { await model.takeTheirs(id) } }.accessibilityIdentifier("block.takeTheirs")
                        }
                    }
                }
            }
            .font(.caption)
            .buttonStyle(.borderless)
            .padding(10)
            .frame(maxWidth: Theme.readingWidth, alignment: .leading)
            .background(Theme.rose.opacity(0.08), in: .rect(cornerRadius: 10, style: .continuous))
            .padding(.horizontal, Theme.gutter)
            .padding(.vertical, 6)
        }
    }
}

/// The `[[` suggestions, above the caret (below it near the top).
private struct CompletionOverlay: View {
    let model: SurfaceModel

    var body: some View {
        GeometryReader { geo in
            if let c = model.completion {
                let frame = geo.frame(in: .global)
                let width = min(340, frame.width - 32)
                let rows = max(1, min(c.results.count, 5))
                let height = CGFloat(rows) * 48 + 10
                let x = min(max(16, c.anchor.minX - frame.minX - 16), frame.width - width - 16)
                let roomAbove = c.anchor.minY - frame.minY
                let y = roomAbove > height + 60 ? roomAbove - height - 8 : c.anchor.maxY - frame.minY + 8
                CompletionList(completion: c, onPick: { model.acceptCompletion($0) })
                    .frame(width: width, height: height)
                    .offset(x: x, y: y)
            }
        }
    }
}

private struct CompletionList: View {
    let completion: SurfaceModel.Completion
    let onPick: (WikiCandidate) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if completion.results.isEmpty {
                Text("No doc matches “\(completion.query)”")
                    .font(.subheadline)
                    .foregroundStyle(Theme.secondary)
                    .padding(12)
            }
            ForEach(Array(completion.results.prefix(5).enumerated()), id: \.element.id) { i, doc in
                Button { onPick(doc) } label: {
                    VStack(alignment: .leading, spacing: 1) {
                        Text(doc.title).font(.subheadline.weight(.medium)).foregroundStyle(Theme.text).lineLimit(1)
                        if let crumb = doc.breadcrumb {
                            Text(crumb).font(.caption2).foregroundStyle(Theme.secondary).lineLimit(1).truncationMode(.head)
                        }
                    }
                    .frame(maxWidth: .infinity, minHeight: 48, alignment: .leading)
                    .padding(.horizontal, 12)
                    .background(i == completion.selected ? Theme.accent.opacity(0.18) : .clear)
                    .contentShape(.rect)
                }
                .buttonStyle(.plain)
                .accessibilityLabel(doc.breadcrumb.map { "\(doc.title), in \($0)" } ?? doc.title)
                .accessibilityIdentifier("wiki.suggestion")
            }
        }
        .padding(.vertical, 5)
        .frame(maxWidth: .infinity, alignment: .topLeading)
        .background(Theme.surface, in: .rect(cornerRadius: 12, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 12, style: .continuous).strokeBorder(Theme.hairline))
        .shadow(color: .black.opacity(0.3), radius: 12, y: 4)
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Doc suggestions")
    }
}

/// The sync chip in the nav bar: what happened to what you typed.
struct EditorChipView: View {
    let chip: EditorChip
    var onRetry: () -> Void = {}

    var color: Color {
        switch chip.tone {
        case .saved: Theme.green
        case .busy: Theme.accent
        case .offline, .review: Theme.amber
        case .conflict, .failed: Theme.rose
        }
    }

    var body: some View {
        let label = HStack(spacing: 6) {
            Circle().fill(color).frame(width: 7, height: 7)
            Text(chip.text).font(.caption.weight(.medium)).foregroundStyle(color).lineLimit(1).minimumScaleFactor(0.8)
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 5)
        .background(color.opacity(0.12), in: .capsule)
        Group {
            if chip.tone == .failed {
                Menu {
                    Button("Try again", systemImage: "arrow.clockwise", action: onRetry)
                } label: { label }
            } else {
                label
            }
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Sync: \(chip.text)")
        .accessibilityIdentifier("editor.chip")
    }
}
