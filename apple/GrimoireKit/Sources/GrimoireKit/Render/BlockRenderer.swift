import Foundation
import Markdown

/// Block markdown → `RenderNode`s, via swift-markdown (cmark-gfm: tables,
/// task lists, strikethrough).
public enum BlockRenderer {
    static let diagramLanguages: Set<String> = ["mermaid", "vega-lite", "vegalite", "vega", "d2"]

    public static func render(_ block: Block) -> [RenderNode] {
        switch block.blockType {
        case .diagramMermaid:
            return [.diagram(kind: "mermaid", source: fenceBody(block.content))]
        case .diagramD2:
            return [.diagram(kind: "d2", source: fenceBody(block.content))]
        case .canvasScene:
            return [.diagram(kind: "canvas", source: "")]
        case .code where block.content.hasPrefix("---\n") && block.content.hasSuffix("---"):
            return [.frontmatter(block.content)]
        default:
            return render(markdown: block.content)
        }
    }

    public static func render(_ records: [BlockRecord]) -> [RenderedBlock] {
        records.map { RenderedBlock(id: $0.id, depth: $0.depth, nodes: render($0.block)) }
    }

    public static func render(markdown: String) -> [RenderNode] {
        Document(parsing: markdown).children.compactMap(node)
    }

    static func node(_ m: any Markup) -> RenderNode? {
        switch m {
        case let h as Heading:
            return .heading(level: h.level, inline: inline(h))
        case let p as Paragraph:
            return .paragraph(inline: inline(p))
        case let l as UnorderedList:
            return .list(ordered: false, start: 1, items: l.listItems.map(item))
        case let l as OrderedList:
            return .list(ordered: true, start: Int(l.startIndex), items: l.listItems.map(item))
        case let q as BlockQuote:
            return quote(q)
        case let c as CodeBlock:
            let lang = c.language?.lowercased()
            let code = c.code.hasSuffix("\n") ? String(c.code.dropLast()) : c.code
            if let lang, diagramLanguages.contains(lang) {
                return .diagram(kind: lang == "vegalite" ? "vega-lite" : lang, source: code)
            }
            return .code(language: c.language, code: code)
        case let t as Markdown.Table:
            return .table(table(t))
        case is ThematicBreak:
            return .thematicBreak
        case let h as HTMLBlock:
            return .html(h.rawHTML)
        default:
            return nil
        }
    }

    /// Inline children re-serialised on their own: formatting them in place
    /// would carry container syntax (`> ` inside quotes, list indents).
    static func inline(_ m: any Markup) -> String {
        Paragraph(m.children.compactMap { $0 as? any InlineMarkup }).format()
    }

    static func item(_ li: Markdown.ListItem) -> RenderNode.ListItem {
        let checked: Bool? = li.checkbox.map { $0 == .checked }
        return RenderNode.ListItem(checked: checked, children: li.children.compactMap(node))
    }

    /// `> [!INFO]` on the first line marks an Obsidian/GitHub-style callout.
    static func quote(_ q: BlockQuote) -> RenderNode {
        var children = q.children.compactMap(node)
        var callout: String?
        if case .paragraph(let text)? = children.first,
           text.hasPrefix("[!"), let close = text.firstIndex(of: "]") {
            callout = String(text[text.index(text.startIndex, offsetBy: 2)..<close]).uppercased()
            let rest = text[text.index(after: close)...].trimmingCharacters(in: .whitespacesAndNewlines)
            if rest.isEmpty {
                children.removeFirst()
            } else {
                children[0] = .paragraph(inline: rest)
            }
        }
        return .quote(callout: callout, children: children)
    }

    static func table(_ t: Markdown.Table) -> RenderNode.Table {
        let header = t.head.cells.map { inline($0) }
        let rows = t.body.rows.map { row in Array(row.cells.map { inline($0) }) }
        let alignments: [RenderNode.Table.Alignment] = t.columnAlignments.map {
            switch $0 {
            case .center: .center
            case .right: .trailing
            default: .leading
            }
        }
        return RenderNode.Table(header: Array(header), rows: Array(rows), alignments: alignments)
    }

    /// The source inside a ```` ```lang ```` fence, or the content as-is.
    static func fenceBody(_ s: String) -> String {
        var lines = s.split(separator: "\n", omittingEmptySubsequences: false)
        guard let first = lines.first, first.hasPrefix("```") else { return s }
        lines.removeFirst()
        if let last = lines.last, last.trimmingCharacters(in: .whitespaces).hasPrefix("```") { lines.removeLast() }
        return lines.joined(separator: "\n")
    }
}
