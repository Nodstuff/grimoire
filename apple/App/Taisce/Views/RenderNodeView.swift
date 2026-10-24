import SwiftUI
import TaisceKit

/// Ticks a task-list checkbox in the enclosing block: (index, new value).
struct CheckboxToggle {
    var overrides: [Int: Bool] = [:]
    var action: ((Int, Bool) -> Void)?
}

extension EnvironmentValues {
    @Entry var checkboxToggle = CheckboxToggle()
}

/// One `RenderNode`, styled. `checkboxBase` is the index of the first
/// task checkbox inside this node within its block (document order), so a
/// tap can address the right `[ ]` in the block's markdown.
struct RenderNodeView: View {
    let node: RenderNode
    var checkboxBase = 0

    var body: some View {
        switch node {
        case let .heading(level, inline):
            Text(InlineMarkdown.attributed(inline))
                .headingFont(level)
                .foregroundStyle(Color.lapis.ink)
                .padding(.top, level <= 2 ? 14 : 8)
                .accessibilityAddTraits(.isHeader)
        case let .paragraph(inline):
            Paragraph(inline: inline)
        case let .list(ordered, start, items):
            ListBlock(ordered: ordered, start: start, items: items, checkboxBase: checkboxBase)
        case let .quote(callout, children):
            if let callout {
                CalloutView(kind: callout, children: children, checkboxBase: checkboxBase)
            } else {
                QuoteView(children: children, checkboxBase: checkboxBase)
            }
        case let .code(language, code, attributes):
            RunnableCodeBlock(language: language, code: code, attributes: attributes)
        case let .table(table):
            TableBlock(table: table)
        case let .diagram(kind, source):
            DiagramBlock(kind: kind, source: source)
        case let .frontmatter(text):
            CodeBlockView(label: "properties", code: text)
        case .thematicBreak:
            Rectangle().fill(Color.lapis.line).frame(height: 1).padding(.vertical, 8)
        case let .html(raw):
            Text(raw).docFont(.caption, design: .monospaced).foregroundStyle(Color.lapis.ink3)
        }
    }

}

extension View {
    /// A heading's style at the doc's text size: SF Pro bold, as the
    /// editor draws it (`SurfaceLook.headingFont`).
    func headingFont(_ level: Int) -> some View {
        switch level {
        case 1: AnyView(docFont(.title2, weight: .bold).tracking(-0.3))
        case 2: AnyView(docFont(.title3, weight: .bold).tracking(-0.3))
        case 3: AnyView(docFont(.headline, weight: .semibold))
        default: AnyView(docFont(.subheadline, weight: .semibold))
        }
    }
}

/// Nodes in order, each told where its checkboxes start.
struct NodeStack: View {
    let nodes: [RenderNode]
    var checkboxBase = 0
    var spacing: CGFloat = 10

    var body: some View {
        VStack(alignment: .leading, spacing: spacing) {
            ForEach(Array(RenderMetrics.checkboxOffsets(nodes, base: checkboxBase).enumerated()), id: \.offset) { i, base in
                RenderNodeView(node: nodes[i], checkboxBase: base)
            }
        }
    }
}

private struct Paragraph: View {
    let inline: String

    var body: some View {
        Text(InlineMarkdown.attributed(inline))
            .lapisFont(.docBody)
            .foregroundStyle(Color.lapis.ink)
            .tint(Color.lapis.accent)
            .frame(maxWidth: .infinity, alignment: .leading)
            .textSelection(.enabled)
    }
}

private struct ListBlock: View {
    @Environment(\.checkboxToggle) private var toggle
    let ordered: Bool
    let start: Int
    let items: [RenderNode.ListItem]
    let checkboxBase: Int

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            let offsets = RenderMetrics.itemOffsets(items, base: checkboxBase)
            ForEach(Array(items.enumerated()), id: \.offset) { i, item in
                let base = offsets[i]
                HStack(alignment: .firstTextBaseline, spacing: 10) {
                    marker(i, item, index: base)
                    NodeStack(nodes: item.children, checkboxBase: base + (item.checked == nil ? 0 : 1), spacing: 6)
                        .opacity(isChecked(item, base) ? 0.6 : 1)
                }
            }
        }
    }

    func isChecked(_ item: RenderNode.ListItem, _ index: Int) -> Bool {
        guard let c = item.checked else { return false }
        return toggle.overrides[index] ?? c
    }

    @ViewBuilder func marker(_ i: Int, _ item: RenderNode.ListItem, index: Int) -> some View {
        if item.checked != nil {
            let checked = isChecked(item, index)
            Button {
                toggle.action?(index, !checked)
            } label: {
                Image(systemName: checked ? "checkmark.square.fill" : "square")
                    .docFont(.body)
                    .foregroundStyle(checked ? Color.lapis.green : Color.lapis.ink3)
                    .frame(width: 28, height: 28)
                    .contentShape(.rect)
            }
            .buttonStyle(.plain)
            .disabled(toggle.action == nil)
            .frame(width: 28)
            .accessibilityLabel(checked ? "Done" : "Not done")
            .accessibilityHint(toggle.action == nil ? "" : "Toggles this item")
        } else if ordered {
            Text("\(start + i).").monospacedDigit().foregroundStyle(Color.lapis.ink3).frame(minWidth: 20, alignment: .trailing)
        } else {
            Circle().fill(Color.lapis.ink3).frame(width: 5, height: 5).frame(width: 20).alignmentGuide(.firstTextBaseline) { $0[.bottom] + 5 }
        }
    }
}

private struct QuoteView: View {
    let children: [RenderNode]
    let checkboxBase: Int

    var body: some View {
        NodeStack(nodes: children, checkboxBase: checkboxBase, spacing: 6)
            .italic()
            .foregroundStyle(Color.lapis.ink3)
            .padding(.leading, 14)
            .overlay(alignment: .leading) {
                RoundedRectangle(cornerRadius: 1.5).fill(Color.lapis.accentRing).frame(width: 3)
            }
    }
}

private struct CalloutView: View {
    let kind: String
    let children: [RenderNode]
    let checkboxBase: Int

    var style: (icon: String, color: Color) {
        switch kind {
        case "WARNING", "CAUTION", "DANGER": ("exclamationmark.triangle", Color.lapis.red)
        case "TIP", "SUCCESS": ("lightbulb", Color.lapis.green)
        case "IMPORTANT", "TODO": ("exclamationmark.circle", Color.lapis.yellow)
        default: ("info.circle", Color.lapis.accent)
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Label(kind.capitalized, systemImage: style.icon)
                .docFont(.subheadline, weight: .semibold)
                .foregroundStyle(style.color)
            NodeStack(nodes: children, checkboxBase: checkboxBase, spacing: 6)
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(style.color.opacity(0.10), in: .rect(cornerRadius: Lapis.Radius.card, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Lapis.Radius.card, style: .continuous).strokeBorder(style.color.opacity(0.35)))
    }
}

struct CodeBlockView: View {
    let label: String?
    let code: String

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if let label, !label.isEmpty {
                Text(label)
                    .docFont(.caption, weight: .medium)
                    .foregroundStyle(Color.lapis.ink3)
                    .padding(.horizontal, 14)
                    .padding(.top, 10)
            }
            ScrollView(.horizontal, showsIndicators: false) {
                Text(code)
                    .docFont(.footnote, design: .monospaced)
                    .foregroundStyle(Color.lapis.ink)
                    .textSelection(.enabled)
                    .fixedSize()
                    .padding(14)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .card(Color.lapis.code, radius: Lapis.Radius.field, elevated: false)
        .accessibilityLabel("Code\(label.map { ", \($0)" } ?? ""): \(code)")
    }
}

private struct TableBlock: View {
    let table: RenderNode.Table

    var columns: Int { max(table.header.count, table.rows.map(\.count).max() ?? 0) }

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            Grid(alignment: .leading, horizontalSpacing: 0, verticalSpacing: 0) {
                GridRow {
                    ForEach(0..<columns, id: \.self) { i in
                        cell(i < table.header.count ? table.header[i] : "", column: i)
                            .docFont(.subheadline, weight: .semibold)
                            .background { Rectangle().fill(Color.lapis.card2) }
                    }
                }
                ForEach(Array(table.rows.enumerated()), id: \.offset) { _, row in
                    Rectangle().fill(Color.lapis.line).frame(height: 1).gridCellUnsizedAxes(.horizontal)
                    GridRow {
                        ForEach(0..<columns, id: \.self) { i in
                            cell(i < row.count ? row[i] : "", column: i).docFont(.subheadline)
                        }
                    }
                }
            }
            .card()
            .clipShape(.rect(cornerRadius: Lapis.Radius.card, style: .continuous))
        }
    }

    func cell(_ inline: String, column: Int) -> some View {
        Text(InlineMarkdown.attributed(inline))
            .foregroundStyle(Color.lapis.ink)
            .tint(Color.lapis.accent)
            .padding(.horizontal, 12)
            .padding(.vertical, 9)
            .frame(maxWidth: .infinity, alignment: alignment(column))
            .gridColumnAlignment(horizontal(column))
    }

    func horizontal(_ i: Int) -> HorizontalAlignment {
        switch table.alignments.indices.contains(i) ? table.alignments[i] : .leading {
        case .leading: .leading
        case .center: .center
        case .trailing: .trailing
        }
    }

    func alignment(_ i: Int) -> Alignment {
        switch table.alignments.indices.contains(i) ? table.alignments[i] : .leading {
        case .leading: .leading
        case .center: .center
        case .trailing: .trailing
        }
    }
}
