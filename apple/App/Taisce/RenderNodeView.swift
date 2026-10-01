import TaisceKit
import SwiftUI

/// One `RenderNode`. Deliberately plain: the design pass styles these.
struct RenderNodeView: View {
    let node: RenderNode

    var body: some View {
        switch node {
        case let .heading(level, inline):
            Text(InlineMarkdown.attributed(inline))
                .font(Self.headingFont(level))
                .padding(.top, level <= 2 ? 8 : 4)
        case let .paragraph(inline):
            Text(InlineMarkdown.attributed(inline))
        case let .list(ordered, start, items):
            ListView(ordered: ordered, start: start, items: items)
        case let .quote(callout, children):
            QuoteView(callout: callout, children: children)
        case let .code(language, code):
            CodeView(label: language, code: code)
        case let .table(table):
            TableView(table: table)
        case let .diagram(kind, source):
            DiagramPlaceholder(kind: kind, source: source)
        case let .frontmatter(text):
            CodeView(label: "properties", code: text)
        case .thematicBreak:
            Divider()
        case let .html(raw):
            Text(raw).font(.caption.monospaced()).foregroundStyle(.secondary)
        }
    }

    static func headingFont(_ level: Int) -> Font {
        switch level {
        case 1: .largeTitle.bold()
        case 2: .title2.bold()
        case 3: .title3.bold()
        default: .headline
        }
    }
}

private struct ListView: View {
    let ordered: Bool
    let start: Int
    let items: [RenderNode.ListItem]

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            ForEach(Array(items.enumerated()), id: \.offset) { i, item in
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    marker(i, item)
                    VStack(alignment: .leading, spacing: 4) {
                        ForEach(Array(item.children.enumerated()), id: \.offset) { _, child in
                            RenderNodeView(node: child)
                        }
                    }
                }
            }
        }
    }

    @ViewBuilder func marker(_ i: Int, _ item: RenderNode.ListItem) -> some View {
        if let checked = item.checked {
            Image(systemName: checked ? "checkmark.square.fill" : "square")
                .foregroundStyle(checked ? Color.accentColor : .secondary)
                .accessibilityLabel(checked ? "done" : "open")
        } else if ordered {
            Text("\(start + i).").monospacedDigit().foregroundStyle(.secondary)
        } else {
            Text("•").foregroundStyle(.secondary)
        }
    }
}

private struct QuoteView: View {
    let callout: String?
    let children: [RenderNode]

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            if let callout {
                Text(callout).font(.caption.bold()).foregroundStyle(.tint)
            }
            ForEach(Array(children.enumerated()), id: \.offset) { _, child in
                RenderNodeView(node: child)
            }
        }
        .padding(.leading, 12)
        .overlay(alignment: .leading) {
            Rectangle().fill(callout == nil ? Color.secondary.opacity(0.4) : Color.accentColor).frame(width: 3)
        }
    }
}

private struct CodeView: View {
    let label: String?
    let code: String

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            if let label, !label.isEmpty {
                Text(label).font(.caption2).foregroundStyle(.secondary)
            }
            ScrollView(.horizontal) {
                Text(code).font(.callout.monospaced()).textSelection(.enabled).fixedSize()
            }
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.quaternary.opacity(0.5), in: .rect(cornerRadius: 8))
    }
}

private struct TableView: View {
    let table: RenderNode.Table

    var body: some View {
        ScrollView(.horizontal) {
            Grid(alignment: .leading, horizontalSpacing: 16, verticalSpacing: 6) {
                GridRow {
                    ForEach(Array(table.header.enumerated()), id: \.offset) { i, cell in
                        Text(InlineMarkdown.attributed(cell)).bold().gridColumnAlignment(alignment(i))
                    }
                }
                Divider()
                ForEach(Array(table.rows.enumerated()), id: \.offset) { _, row in
                    GridRow {
                        ForEach(Array(row.enumerated()), id: \.offset) { _, cell in
                            Text(InlineMarkdown.attributed(cell))
                        }
                    }
                }
            }
            .padding(.vertical, 4)
        }
    }

    func alignment(_ i: Int) -> HorizontalAlignment {
        switch table.alignments.indices.contains(i) ? table.alignments[i] : .leading {
        case .leading: .leading
        case .center: .center
        case .trailing: .trailing
        }
    }
}

/// Mermaid / Vega-Lite / D2 render later (offscreen WKWebView for Mermaid).
private struct DiagramPlaceholder: View {
    let kind: String
    let source: String

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Label("\(kind) diagram", systemImage: "chart.xyaxis.line").font(.subheadline.bold())
            Text(source).font(.caption.monospaced()).foregroundStyle(.secondary).lineLimit(6)
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.tint.opacity(0.08), in: .rect(cornerRadius: 10))
        .overlay(RoundedRectangle(cornerRadius: 10).strokeBorder(.tint.opacity(0.3)))
    }
}
