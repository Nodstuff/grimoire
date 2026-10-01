import Foundation
import Testing
import TaisceKit
import UIKit
@testable import Taisce

private struct HangingRenderer: DiagramRendering {
    func render(_ request: DiagramRequest) async throws -> Data {
        try await Task.sleep(for: .seconds(30))
        return Data()
    }
}

private struct FailingRenderer: DiagramRendering {
    func render(_ request: DiagramRequest) async throws -> Data {
        throw DiagramRenderError(message: "Parse error on line 2:\n...A-->\n-----^")
    }
}

@Suite struct DiagramPhaseTests {
    @Test func timeoutBecomesTheErrorCard() async {
        let q = DiagramRenderQueue(renderer: HangingRenderer(), store: MemoryDiagramStore(), timeout: .milliseconds(200))
        let phase = await DiagramPhase.load(DiagramRequest(source: "graph LR; A-->B", theme: .dark, width: 320), queue: q)
        #expect(phase == .failed("The diagram took longer than 0.2 s to draw."))
    }

    @Test func mermaidErrorIsShown() async {
        let q = DiagramRenderQueue(renderer: FailingRenderer(), store: MemoryDiagramStore())
        let phase = await DiagramPhase.load(DiagramRequest(source: "graph LR; A-->", theme: .light, width: 320), queue: q)
        #expect(phase == .failed("Parse error on line 2:\n...A-->\n-----^"))
    }

    @Test func widthBuckets() {
        #expect(DiagramPhase.bucket(359.7) == 340)
        #expect(DiagramPhase.bucket(360) == 360)
        #expect(DiagramPhase.bucket(20) == 160)
    }

    @Test func mermaidErrorsAreTrimmed() {
        #expect(MermaidWebView.tidy("a\n\n  b  \nc\nd\ne") == "a\nb\nc\nd")
    }

    @Test func docTablesComeFromThePage() {
        let blocks = [
            Block(id: "01a080ba-17b1-7a43-93d5-4d99f46f2abc", docID: "d", parentID: nil, orderKey: "a", blockType: .paragraph, content: "| m | v |\n|---|---|\n| Jan | 3 |", epoch: 1, deleted: false),
            Block(id: "01a080ba-17b1-7a43-93d5-4d99f46f2abd", docID: "d", parentID: nil, orderKey: "b", blockType: .code, content: "```vega-lite\n{\"data\": {\"block\": \"^6f2abc\"}, \"mark\": \"bar\", \"encoding\": {\"x\": {\"field\": \"m\"}, \"y\": {\"field\": \"v\"}}}\n```", epoch: 1, deleted: false),
        ]
        let page = DocPage.build(title: "T", blocks: blocks)
        let tables = page.chartTables
        #expect(tables.table(ref: "^6f2abc")?.rows == [["Jan", "3"]])
        guard case let .diagram(kind, source)? = page.blocks.last?.nodes.first else {
            Issue.record("expected a diagram node")
            return
        }
        #expect(kind == "vega-lite")
        guard case let .chart(model) = ChartSpec.parse(source, tables: tables) else {
            Issue.record("expected a chart")
            return
        }
        #expect(model.layers.first?.points.first?.y == .number(3))
    }

    @Test func paletteStartsWithTheAccent() {
        #expect(ChartPalette.colors(for: ["a", "b"]).first == Theme.accent)
        #expect(ChartPalette.colors(for: Array(repeating: "s", count: 10)).count == 10)
    }
}

/// One real render through the bundled mermaid in the shared web view.
@MainActor
@Suite struct MermaidSmokeTests {
    @Test func flowchartRendersToAnImage() async throws {
        let q = DiagramRenderQueue(renderer: MermaidWebRenderer(), store: MemoryDiagramStore(), timeout: .seconds(30))
        let req = DiagramRequest(source: "flowchart LR\n  A[Phone] --> B(Cache)\n  B --> C{Server}", theme: .dark, width: 320, scale: 2)
        let phase = await DiagramPhase.load(req, queue: q)
        guard case let .image(image) = phase else {
            Issue.record("expected an image, got \(phase)")
            return
        }
        #expect(image.size.width > 50 && image.size.height > 10)
        #expect(image.size.width <= 320)
        #expect(image.scale == 2)

        let bad = await DiagramPhase.load(DiagramRequest(source: "flowchart LR\n  A -->", theme: .light, width: 320, scale: 2), queue: q)
        guard case let .failed(message) = bad else {
            Issue.record("expected a parse error, got \(bad)")
            return
        }
        #expect(!message.isEmpty)
    }
}
