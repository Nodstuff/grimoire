import SwiftUI
import TaisceKit

/// A Claude reply, drawn from its parsed blocks (`ReplyNode`) in the chat's
/// type: SF Pro body, headings a step up, lists to any depth, code in SF
/// Mono on the code ground, quotes with the accent bar, tables in a card.
/// Nothing is parsed here: the feed built the nodes once.
struct ReplyView: View {
    let nodes: [ReplyNode]

    var body: some View {
        ReplyBlocks(nodes: nodes, spacing: 10)
            .foregroundStyle(Color.lapis.ink)
            .tint(Color.lapis.accent)
            .textSelection(.enabled)
            .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// Blocks in order.
private struct ReplyBlocks: View {
    let nodes: [ReplyNode]
    var spacing: CGFloat = 8

    var body: some View {
        VStack(alignment: .leading, spacing: spacing) {
            ForEach(nodes.indices, id: \.self) { i in
                ReplyNodeView(node: nodes[i])
            }
        }
    }
}

private struct ReplyNodeView: View {
    let node: ReplyNode

    var body: some View {
        switch node {
        case let .heading(level, text):
            Text(text)
                .font(Self.headingFont(level))
                .padding(.top, level <= 2 ? 6 : 2)
                .frame(maxWidth: .infinity, alignment: .leading)
                .accessibilityAddTraits(.isHeader)
        case let .paragraph(text):
            Text(text)
                .font(.body)
                .frame(maxWidth: .infinity, alignment: .leading)
                .fixedSize(horizontal: false, vertical: true)
        case let .list(ordered, start, items):
            VStack(alignment: .leading, spacing: 6) {
                ForEach(items.indices, id: \.self) { i in
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        marker(ordered: ordered, number: start + i, checked: items[i].checked)
                        ReplyBlocks(nodes: items[i].children, spacing: 6)
                    }
                }
            }
        case let .quote(children):
            ReplyBlocks(nodes: children, spacing: 6)
                .foregroundStyle(Color.lapis.ink2)
                .padding(.leading, 12)
                .overlay(alignment: .leading) {
                    RoundedRectangle(cornerRadius: 1.5).fill(Color.lapis.accentRing).frame(width: 3)
                }
        case let .code(language, code):
            ReplyCode(language: language, code: code)
        case let .table(table):
            ReplyTableView(table: table)
        case .rule:
            Rectangle().fill(Color.lapis.line).frame(height: 1).padding(.vertical, 4)
        }
    }

    static func headingFont(_ level: Int) -> Font {
        switch level {
        case 1: .title3.weight(.semibold)
        case 2: .headline
        default: .subheadline.weight(.semibold)
        }
    }

    @ViewBuilder func marker(ordered: Bool, number: Int, checked: Bool?) -> some View {
        if let checked {
            Image(systemName: checked ? "checkmark.square.fill" : "square")
                .font(.body)
                .foregroundStyle(checked ? Color.lapis.green : Color.lapis.ink3)
                .accessibilityLabel(checked ? "Done" : "Not done")
        } else if ordered {
            Text("\(number).").font(.body).monospacedDigit().foregroundStyle(Color.lapis.ink3).frame(minWidth: 18, alignment: .trailing)
        } else {
            Text("\u{2022}").font(.body).foregroundStyle(Color.lapis.ink3).frame(minWidth: 10)
        }
    }
}

/// A fenced block: SF Mono on the code ground, scrolling sideways rather
/// than wrapping, its language a small label.
private struct ReplyCode: View {
    let language: String?
    let code: String

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if let language, !language.isEmpty {
                Text(language)
                    .font(.caption2.weight(.medium))
                    .foregroundStyle(Color.lapis.ink3)
                    .padding(.horizontal, 12)
                    .padding(.top, 8)
            }
            ScrollView(.horizontal, showsIndicators: false) {
                Text(code)
                    .font(.system(.footnote, design: .monospaced))
                    .foregroundStyle(Color.lapis.ink)
                    .fixedSize()
                    .padding(.horizontal, 12)
                    .padding(.vertical, 10)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.lapis.code, in: .rect(cornerRadius: Lapis.Radius.field, style: .continuous))
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Code\(language.map { ", \($0)" } ?? ""): \(code)")
    }
}

private struct ReplyTableView: View {
    let table: ReplyTable

    var columns: Int { max(table.header.count, table.rows.map(\.count).max() ?? 0) }

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            Grid(alignment: .leading, horizontalSpacing: 0, verticalSpacing: 0) {
                GridRow {
                    ForEach(0..<columns, id: \.self) { i in
                        cell(i < table.header.count ? table.header[i] : "", column: i)
                            .font(.subheadline.weight(.semibold))
                            .background(Color.lapis.card2)
                    }
                }
                ForEach(table.rows.indices, id: \.self) { r in
                    Rectangle().fill(Color.lapis.line).frame(height: 1).gridCellUnsizedAxes(.horizontal)
                    GridRow {
                        ForEach(0..<columns, id: \.self) { i in
                            cell(i < table.rows[r].count ? table.rows[r][i] : "", column: i).font(.subheadline)
                        }
                    }
                }
            }
            .background(Color.lapis.card)
            .clipShape(.rect(cornerRadius: Lapis.Radius.field, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: Lapis.Radius.field, style: .continuous).strokeBorder(Color.lapis.line))
        }
    }

    func cell(_ text: AttributedString, column: Int) -> some View {
        let a = table.alignments.indices.contains(column) ? table.alignments[column] : .leading
        return Text(text)
            .padding(.horizontal, 10)
            .padding(.vertical, 7)
            .frame(maxWidth: .infinity, alignment: a == .center ? .center : a == .trailing ? .trailing : .leading)
            .gridColumnAlignment(a == .center ? .center : a == .trailing ? .trailing : .leading)
    }
}

#Preview("Reply · light") {
    ScrollView {
        ReplyView(nodes: ReplyMarkdown.parse("""
        ## What changed

        The **deadline** now moves only when a message arrives. See `grpc.rs`.

        1. First
        2. Second
           - nested *one*
           - nested two

        ```swift
        let x = 1
        ```

        > A quote, with a [link](https://example.org).

        | a | b |
        |---|--:|
        | 1 | 2 |
        """))
        .padding()
    }
    .groundBackground()
    .preferredColorScheme(.light)
}
