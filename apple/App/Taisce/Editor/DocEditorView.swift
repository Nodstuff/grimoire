import SwiftUI
import TaisceKit

/// Edit mode: one text view per block, in document order, with review
/// marks and the `[[` popup.
struct DocEditorView: View {
    let title: String
    let model: EditorModel
    @Environment(\.dynamicTypeSize) private var dynamicType

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                VStack(alignment: .leading, spacing: 4) {
                    Text(title)
                        .font(Theme.serif(.largeTitle))
                        .foregroundStyle(Theme.text)
                        .accessibilityAddTraits(.isHeader)
                        .padding(.bottom, 10)
                    LazyVStack(alignment: .leading, spacing: 4) {
                        ForEach(model.items) { item in
                            BlockRow(item: item, model: model, review: model.reviews[item.id], dynamicType: dynamicType)
                                .id(item.id)
                        }
                    }
                    // the space below the last block adds a paragraph there
                    Color.clear
                        .frame(maxWidth: .infinity, minHeight: 200)
                        .contentShape(.rect)
                        .onTapGesture { model.tapAtEnd() }
                        .accessibilityElement()
                        .accessibilityLabel("Add a paragraph at the end")
                        .accessibilityAddTraits(.isButton)
                        .accessibilityIdentifier("editor.end")
                }
                .padding(.horizontal, Theme.gutter)
                .padding(.top, 4)
                .frame(maxWidth: Theme.readingWidth, alignment: .leading)
                .frame(maxWidth: .infinity)
            }
            .scrollDismissesKeyboard(.interactively)
            // the [[ popup is pinned to where the caret was: a scroll drops it
            .onScrollPhaseChange { _, phase in
                if phase == .interacting { model.dismissCompletion() }
            }
            .onChange(of: model.pendingFocus) { _, f in
                if let f { proxy.scrollTo(f.id) }
            }
        }
        .groundBackground()
        .overlay { CompletionOverlay(model: model) }
    }
}

/// One block: its text view, outlined when the gate flagged or parked our edit.
private struct BlockRow: View {
    let item: EditorSession.Item
    let model: EditorModel
    let review: BlockReview?
    let dynamicType: DynamicTypeSize
    @State private var layoutToken = 0

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            BlockEditorView(item: item, model: model, dynamicType: dynamicType, layoutToken: layoutToken) {
                layoutToken &+= 1
            }
            .padding(.top, Self.topPadding(item.content))
            if let review {
                Label(review.verdict == .red ? "Waiting for review on desktop" : "Flagged for review on desktop",
                      systemImage: review.verdict == .red ? "hourglass" : "flag")
                    .font(.caption)
                    .foregroundStyle(review.verdict == .red ? Theme.rose : Theme.amber)
                    .padding(.bottom, 4)
            }
        }
        .padding(.horizontal, review == nil ? 0 : 10)
        .overlay {
            if let review {
                RoundedRectangle(cornerRadius: 10, style: .continuous)
                    .strokeBorder(review.verdict == .red ? Theme.rose : Theme.amber, lineWidth: 1.5)
                    .allowsHitTesting(false)
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityHint(review.map { $0.verdict == .red ? "Your edit is waiting for review on the desktop" : "Your edit was flagged for review" } ?? "")
    }

    static func topPadding(_ c: EditorBlockContent) -> CGFloat {
        if case let .heading(level, _) = c { return level <= 2 ? 12 : 6 }
        return 0
    }
}

/// The `[[` suggestions, above the caret (below it near the top).
private struct CompletionOverlay: View {
    let model: EditorModel

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
    let completion: EditorModel.Completion
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

/// The edit-mode chip in the nav bar.
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
