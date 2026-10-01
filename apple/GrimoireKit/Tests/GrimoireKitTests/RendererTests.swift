import Foundation
import Testing
@testable import GrimoireKit

@Suite struct RendererTests {
    func block(_ type: BlockType, _ content: String) -> Block {
        Block(id: "b", docID: "d", parentID: nil, orderKey: "i", blockType: type, content: content)
    }

    @Test func headingAndInlineMarkdownSurvive() {
        #expect(BlockRenderer.render(markdown: "## The **four** diseases") == [.heading(level: 2, inline: "The **four** diseases")])
        #expect(BlockRenderer.render(markdown: "See `x` and [[A/B]]") == [.paragraph(inline: "See `x` and [[A/B]]")])
    }

    @Test func taskListsAndNesting() throws {
        let nodes = BlockRenderer.render(markdown: "- [ ] open\n- [x] done\n- plain\n  1. nested")
        guard case .list(false, _, let items)? = nodes.first else {
            Issue.record("not a list: \(nodes)")
            return
        }
        #expect(items.map(\.checked) == [false, true, nil])
        #expect(items[2].children.last.map { if case .list(true, 1, _) = $0 { true } else { false } } == true)
    }

    @Test func calloutQuote() {
        let nodes = BlockRenderer.render(markdown: "> [!INFO]\n> **START HERE.** Read this.")
        #expect(nodes == [.quote(callout: "INFO", children: [.paragraph(inline: "**START HERE.** Read this.")])])
    }

    @Test func gfmTable() {
        let md = "| A | B |\n|---|:-:|\n| 1 | **2** |\n| 3 | 4 |"
        #expect(BlockRenderer.render(markdown: md) == [.table(.init(
            header: ["A", "B"], rows: [["1", "**2**"], ["3", "4"]], alignments: [.leading, .center]
        ))])
    }

    @Test func codeAndDiagramFences() {
        #expect(BlockRenderer.render(block(.code, "```rust\nfn main() {}\n```")) == [.code(language: "rust", code: "fn main() {}")])
        #expect(BlockRenderer.render(block(.diagramMermaid, "```mermaid\ngraph TD\n```")) == [.diagram(kind: "mermaid", source: "graph TD")])
        #expect(BlockRenderer.render(block(.paragraph, "```vega-lite\n{}\n```")) == [.diagram(kind: "vega-lite", source: "{}")])
        #expect(BlockRenderer.render(block(.code, "---\ntitle: x\n---")) == [.frontmatter("---\ntitle: x\n---")])
    }

    @Test func wikiLinksBecomeAppLinks() throws {
        #expect(InlineMarkdown.rewriteWikiLinks("go [[Alert Engine/02 Clamps|clamps]] or [[X]]")
            == "go [clamps](grimoire://wiki/Alert%20Engine%2F02%20Clamps) or [X](grimoire://wiki/X)")
        let url = try #require(URL(string: "grimoire://wiki/Alert%20Engine%2F02%20Clamps"))
        #expect(InlineMarkdown.wikiTarget(url) == "Alert Engine/02 Clamps")
        let s = InlineMarkdown.attributed("a **b** [[C]]")
        #expect(String(s.characters) == "a b C")
    }
}
